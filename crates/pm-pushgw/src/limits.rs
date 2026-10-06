//! Rate limits and temporary source blocks.
//!
//! [`LimitStore`] is the state: expiring counters and expiring blocks,
//! keyed by string. [`Limiter`] is the policy on top of it. A store
//! shared between relay instances only has to implement the three
//! store operations, each of which is a single atomic step.

use std::collections::HashMap;
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use async_trait::async_trait;
use tracing::warn;

pub const DEFAULT_PUSHES_PER_SOURCE: u32 = 60;
pub const DEFAULT_PUSHES_PER_TOKEN: u32 = 30;
pub const DEFAULT_RATE_WINDOW_SECS: u64 = 60;
pub const DEFAULT_BAD_TOKENS_PER_SOURCE: u32 = 5;
pub const DEFAULT_BAD_TOKEN_WINDOW_SECS: u64 = 60;
pub const DEFAULT_BLOCK_SECS: u64 = 600;

/// Stands for "never block" in [`LimitPolicy::bad_tokens_per_source`].
pub const BAD_TOKEN_BLOCK_DISABLED: u32 = 0;

/// Entries [`MemoryStore`] holds per table before it refuses new keys.
const MEMORY_STORE_CAPACITY: usize = 100_000;

const SOURCE_RATE_PREFIX: &str = "rate:source:";
const TOKEN_RATE_PREFIX: &str = "rate:token:";
const BAD_TOKEN_PREFIX: &str = "bad-token:source:";
const BLOCK_PREFIX: &str = "block:source:";

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum StoreError {
    /// The store holds as many live keys as it allows.
    Full,
    /// The store could not be reached or answered with an error.
    Unavailable(String),
}

impl std::fmt::Display for StoreError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Full => f.write_str("limit store is full"),
            Self::Unavailable(detail) => write!(f, "limit store unavailable: {detail}"),
        }
    }
}

/// Expiring counters and blocks. Every operation must be atomic, since
/// concurrent requests hit the same keys.
#[async_trait]
pub trait LimitStore: Send + Sync {
    /// Adds one to the counter at `key` and returns the new total. The
    /// counter expires `window` after the increment that created it.
    async fn increment(&self, key: &str, window: Duration) -> Result<u32, StoreError>;

    /// Blocks `key` for `ttl`, replacing any block already there.
    async fn block(&self, key: &str, ttl: Duration) -> Result<(), StoreError>;

    /// How much longer `key` is blocked, or `None` when it is not.
    async fn blocked_for(&self, key: &str) -> Result<Option<Duration>, StoreError>;
}

/// A [`LimitStore`] in process memory: lost on restart and not shared
/// between instances.
pub struct MemoryStore {
    tables: Mutex<Tables>,
    capacity: usize,
}

#[derive(Default)]
struct Tables {
    counters: HashMap<String, Counter>,
    blocks: HashMap<String, Instant>,
}

struct Counter {
    count: u32,
    expires_at: Instant,
}

impl Default for MemoryStore {
    fn default() -> Self {
        Self::with_capacity(MEMORY_STORE_CAPACITY)
    }
}

impl MemoryStore {
    pub fn with_capacity(capacity: usize) -> Self {
        Self {
            tables: Mutex::new(Tables::default()),
            capacity,
        }
    }

    fn increment_at(&self, key: &str, window: Duration, now: Instant) -> Result<u32, StoreError> {
        let mut tables = self.tables.lock().unwrap();
        if let Some(counter) = tables.counters.get_mut(key) {
            if counter.expires_at > now {
                counter.count = counter.count.saturating_add(1);
                return Ok(counter.count);
            }
        }
        if tables.counters.len() >= self.capacity {
            tables.counters.retain(|_, c| c.expires_at > now);
        }
        if tables.counters.len() >= self.capacity {
            return Err(StoreError::Full);
        }
        tables.counters.insert(
            key.to_string(),
            Counter {
                count: 1,
                expires_at: now + window,
            },
        );
        Ok(1)
    }

    fn block_at(&self, key: &str, ttl: Duration, now: Instant) -> Result<(), StoreError> {
        let mut tables = self.tables.lock().unwrap();
        if !tables.blocks.contains_key(key) && tables.blocks.len() >= self.capacity {
            tables.blocks.retain(|_, until| *until > now);
            if tables.blocks.len() >= self.capacity {
                return Err(StoreError::Full);
            }
        }
        tables.blocks.insert(key.to_string(), now + ttl);
        Ok(())
    }

    fn blocked_for_at(&self, key: &str, now: Instant) -> Option<Duration> {
        let mut tables = self.tables.lock().unwrap();
        let until = *tables.blocks.get(key)?;
        if until > now {
            Some(until - now)
        } else {
            tables.blocks.remove(key);
            None
        }
    }
}

#[async_trait]
impl LimitStore for MemoryStore {
    async fn increment(&self, key: &str, window: Duration) -> Result<u32, StoreError> {
        self.increment_at(key, window, Instant::now())
    }

    async fn block(&self, key: &str, ttl: Duration) -> Result<(), StoreError> {
        self.block_at(key, ttl, Instant::now())
    }

    async fn blocked_for(&self, key: &str) -> Result<Option<Duration>, StoreError> {
        Ok(self.blocked_for_at(key, Instant::now()))
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LimitPolicy {
    /// Pushes one source may send per `window`.
    pub pushes_per_source: u32,
    /// Pushes one device token may receive per `window`.
    pub pushes_per_token: u32,
    pub window: Duration,
    /// Tokens APNs may call invalid, per source and `bad_token_window`,
    /// before the source is blocked. [`BAD_TOKEN_BLOCK_DISABLED`] turns
    /// the block off.
    pub bad_tokens_per_source: u32,
    pub bad_token_window: Duration,
    pub block_duration: Duration,
}

impl Default for LimitPolicy {
    fn default() -> Self {
        Self {
            pushes_per_source: DEFAULT_PUSHES_PER_SOURCE,
            pushes_per_token: DEFAULT_PUSHES_PER_TOKEN,
            window: Duration::from_secs(DEFAULT_RATE_WINDOW_SECS),
            bad_tokens_per_source: DEFAULT_BAD_TOKENS_PER_SOURCE,
            bad_token_window: Duration::from_secs(DEFAULT_BAD_TOKEN_WINDOW_SECS),
            block_duration: Duration::from_secs(DEFAULT_BLOCK_SECS),
        }
    }
}

/// Whether a push may go on to APNs.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Admission {
    Allowed,
    /// The source sent too many invalid device tokens and is blocked
    /// for this much longer.
    Blocked(Duration),
    SourceLimited,
    TokenLimited,
    /// The store failed, and a push that cannot be counted is refused.
    StoreFailed,
}

pub struct Limiter {
    store: Arc<dyn LimitStore>,
    policy: LimitPolicy,
}

impl Limiter {
    pub fn new(store: Arc<dyn LimitStore>, policy: LimitPolicy) -> Self {
        Self { store, policy }
    }

    /// Counts one push from `source` to the token hashed as
    /// `token_hash`. The source is checked first, so a source already
    /// over its limit cannot create token counters.
    pub async fn admit(&self, source: &str, token_hash: &str) -> Admission {
        match self.try_admit(source, token_hash).await {
            Ok(admission) => admission,
            Err(error) => {
                warn!(source, %error, "refusing push, limit store failed");
                Admission::StoreFailed
            }
        }
    }

    async fn try_admit(&self, source: &str, token_hash: &str) -> Result<Admission, StoreError> {
        let block_key = format!("{BLOCK_PREFIX}{source}");
        if let Some(remaining) = self.store.blocked_for(&block_key).await? {
            return Ok(Admission::Blocked(remaining));
        }

        let source_key = format!("{SOURCE_RATE_PREFIX}{source}");
        if self
            .store
            .increment(&source_key, self.policy.window)
            .await?
            > self.policy.pushes_per_source
        {
            return Ok(Admission::SourceLimited);
        }

        let token_key = format!("{TOKEN_RATE_PREFIX}{token_hash}");
        if self.store.increment(&token_key, self.policy.window).await?
            > self.policy.pushes_per_token
        {
            return Ok(Admission::TokenLimited);
        }
        Ok(Admission::Allowed)
    }

    /// Records that APNs called a token from `source` invalid. Returns
    /// the block duration when this one put the source over the limit.
    pub async fn record_bad_token(&self, source: &str) -> Option<Duration> {
        match self.try_record_bad_token(source).await {
            Ok(blocked) => blocked,
            Err(error) => {
                warn!(source, %error, "invalid device token not counted, limit store failed");
                None
            }
        }
    }

    async fn try_record_bad_token(&self, source: &str) -> Result<Option<Duration>, StoreError> {
        if self.policy.bad_tokens_per_source == BAD_TOKEN_BLOCK_DISABLED {
            return Ok(None);
        }
        let count_key = format!("{BAD_TOKEN_PREFIX}{source}");
        let count = self
            .store
            .increment(&count_key, self.policy.bad_token_window)
            .await?;
        if count < self.policy.bad_tokens_per_source {
            return Ok(None);
        }
        let block_key = format!("{BLOCK_PREFIX}{source}");
        self.store
            .block(&block_key, self.policy.block_duration)
            .await?;
        Ok(Some(self.policy.block_duration))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const WINDOW: Duration = Duration::from_secs(60);

    const BLOCK_DURATION: Duration = Duration::from_secs(DEFAULT_BLOCK_SECS);

    fn limiter() -> Limiter {
        Limiter::new(Arc::new(MemoryStore::default()), LimitPolicy::default())
    }

    #[test]
    fn a_counter_counts_up_within_its_window() {
        let store = MemoryStore::default();
        let start = Instant::now();
        assert_eq!(store.increment_at("k", WINDOW, start), Ok(1));
        assert_eq!(store.increment_at("k", WINDOW, start + WINDOW / 2), Ok(2));
        assert_eq!(store.increment_at("other", WINDOW, start), Ok(1));
    }

    /// The window is fixed from the first increment, so later
    /// increments do not extend it.
    #[test]
    fn a_counter_restarts_once_its_window_has_passed() {
        let store = MemoryStore::default();
        let start = Instant::now();
        store.increment_at("k", WINDOW, start).unwrap();
        store.increment_at("k", WINDOW, start + WINDOW / 2).unwrap();
        assert_eq!(store.increment_at("k", WINDOW, start + WINDOW), Ok(1));
    }

    #[test]
    fn a_full_store_refuses_new_keys_but_still_counts_known_ones() {
        let store = MemoryStore::with_capacity(2);
        let start = Instant::now();
        store.increment_at("a", WINDOW, start).unwrap();
        store.increment_at("b", WINDOW, start).unwrap();
        assert_eq!(
            store.increment_at("c", WINDOW, start),
            Err(StoreError::Full)
        );
        assert_eq!(store.increment_at("a", WINDOW, start), Ok(2));
    }

    #[test]
    fn a_full_store_makes_room_from_expired_counters() {
        let store = MemoryStore::with_capacity(2);
        let start = Instant::now();
        store.increment_at("a", WINDOW, start).unwrap();
        store.increment_at("b", WINDOW, start).unwrap();
        assert_eq!(store.increment_at("c", WINDOW, start + WINDOW), Ok(1));
    }

    #[test]
    fn a_block_reports_its_remaining_time_and_then_lapses() {
        let store = MemoryStore::default();
        let start = Instant::now();
        let ttl = Duration::from_secs(600);
        assert_eq!(store.blocked_for_at("k", start), None);
        store.block_at("k", ttl, start).unwrap();
        assert_eq!(
            store.blocked_for_at("k", start + Duration::from_secs(100)),
            Some(Duration::from_secs(500))
        );
        assert_eq!(store.blocked_for_at("k", start + ttl), None);
    }

    #[test]
    fn a_full_block_table_refuses_new_blocks_until_one_lapses() {
        let store = MemoryStore::with_capacity(1);
        let start = Instant::now();
        store.block_at("a", WINDOW, start).unwrap();
        assert_eq!(store.block_at("b", WINDOW, start), Err(StoreError::Full));
        assert_eq!(store.block_at("b", WINDOW, start + WINDOW), Ok(()));
    }

    #[tokio::test]
    async fn a_source_is_limited_after_its_allowance_and_others_are_not() {
        let limiter = limiter();
        for i in 0..DEFAULT_PUSHES_PER_SOURCE {
            let token = format!("token-{i}");
            assert_eq!(limiter.admit("1.2.3.4", &token).await, Admission::Allowed);
        }
        assert_eq!(
            limiter.admit("1.2.3.4", "one-more").await,
            Admission::SourceLimited
        );
        assert_eq!(
            limiter.admit("5.6.7.8", "one-more").await,
            Admission::Allowed
        );
    }

    #[tokio::test]
    async fn a_token_is_limited_after_its_allowance_across_sources() {
        let limiter = limiter();
        for i in 0..DEFAULT_PUSHES_PER_TOKEN {
            let source = format!("10.0.0.{i}");
            assert_eq!(limiter.admit(&source, "token").await, Admission::Allowed);
        }
        assert_eq!(
            limiter.admit("10.9.9.9", "token").await,
            Admission::TokenLimited
        );
        assert_eq!(limiter.admit("10.9.9.9", "other").await, Admission::Allowed);
    }

    /// Otherwise one source could spend every token's allowance, and
    /// fill the store with token counters, after being limited itself.
    #[tokio::test]
    async fn a_limited_source_does_not_spend_a_tokens_allowance() {
        let limiter = Limiter::new(
            Arc::new(MemoryStore::default()),
            LimitPolicy {
                pushes_per_source: 1,
                pushes_per_token: 2,
                ..LimitPolicy::default()
            },
        );
        assert_eq!(limiter.admit("1.2.3.4", "a").await, Admission::Allowed);
        for _ in 0..5 {
            assert_eq!(
                limiter.admit("1.2.3.4", "victim").await,
                Admission::SourceLimited
            );
        }
        assert_eq!(limiter.admit("5.6.7.8", "victim").await, Admission::Allowed);
    }

    #[tokio::test]
    async fn invalid_tokens_block_the_source_once_they_reach_the_limit() {
        let limiter = limiter();
        for _ in 1..DEFAULT_BAD_TOKENS_PER_SOURCE {
            assert_eq!(limiter.record_bad_token("1.2.3.4").await, None);
            assert_eq!(limiter.admit("1.2.3.4", "t").await, Admission::Allowed);
        }
        assert_eq!(
            limiter.record_bad_token("1.2.3.4").await,
            Some(BLOCK_DURATION)
        );
        assert!(matches!(
            limiter.admit("1.2.3.4", "t").await,
            Admission::Blocked(remaining) if remaining <= BLOCK_DURATION
        ));
        assert_eq!(limiter.admit("5.6.7.8", "t").await, Admission::Allowed);
    }

    #[tokio::test]
    async fn a_configured_policy_replaces_the_defaults() {
        let block = Duration::from_secs(30);
        let limiter = Limiter::new(
            Arc::new(MemoryStore::default()),
            LimitPolicy {
                bad_tokens_per_source: 2,
                block_duration: block,
                ..LimitPolicy::default()
            },
        );
        assert_eq!(limiter.record_bad_token("1.2.3.4").await, None);
        assert_eq!(limiter.record_bad_token("1.2.3.4").await, Some(block));
        assert!(matches!(
            limiter.admit("1.2.3.4", "t").await,
            Admission::Blocked(remaining) if remaining <= block
        ));
    }

    #[tokio::test]
    async fn the_invalid_token_block_can_be_turned_off() {
        let limiter = Limiter::new(
            Arc::new(MemoryStore::default()),
            LimitPolicy {
                bad_tokens_per_source: BAD_TOKEN_BLOCK_DISABLED,
                ..LimitPolicy::default()
            },
        );
        for _ in 0..DEFAULT_BAD_TOKENS_PER_SOURCE * 2 {
            assert_eq!(limiter.record_bad_token("1.2.3.4").await, None);
        }
        assert_eq!(limiter.admit("1.2.3.4", "t").await, Admission::Allowed);
    }

    struct FailingStore;

    #[async_trait]
    impl LimitStore for FailingStore {
        async fn increment(&self, _: &str, _: Duration) -> Result<u32, StoreError> {
            Err(StoreError::Unavailable("down".into()))
        }
        async fn block(&self, _: &str, _: Duration) -> Result<(), StoreError> {
            Err(StoreError::Unavailable("down".into()))
        }
        async fn blocked_for(&self, _: &str) -> Result<Option<Duration>, StoreError> {
            Err(StoreError::Unavailable("down".into()))
        }
    }

    /// An open relay with no working limits spends the operator's APNs
    /// credential unbounded, so a failed store refuses pushes.
    #[tokio::test]
    async fn a_failed_store_refuses_pushes() {
        let limiter = Limiter::new(Arc::new(FailingStore), LimitPolicy::default());
        assert_eq!(limiter.admit("1.2.3.4", "t").await, Admission::StoreFailed);
        assert_eq!(limiter.record_bad_token("1.2.3.4").await, None);
    }
}
