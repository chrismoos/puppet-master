use std::time::Duration;

use crate::storage::Storage;

/// The shortest gap between two announcements, so a call moving through
/// several states in a burst reaches clients as one refetch.
pub(crate) const CONNECTIONS_CHANGE_COALESCE: Duration = Duration::from_millis(250);
/// The pause after a failed expiry sweep, so a broken database is not retried
/// in a tight loop.
const EXPIRY_RETRY: Duration = Duration::from_secs(1);

/// Calls `announce` after connection or call rows change, at most once per
/// [`CONNECTIONS_CHANGE_COALESCE`], and expires approvals when their deadline
/// passes so clients learn of it without asking. Runs until dropped.
pub(crate) async fn relay_connection_changes(storage: &Storage, mut announce: impl FnMut()) {
    let changed = storage.connections_changed();
    loop {
        let deadline = match storage.next_connection_call_expiry() {
            Ok(next) => next,
            Err(error) => {
                tracing::warn!(%error, "could not read the next connection call expiry");
                None
            }
        };
        let until_deadline = deadline.map(|at| {
            Duration::from_millis(u64::try_from(at - crate::storage::now_unix_ms()).unwrap_or(0))
        });
        tokio::select! {
            () = changed.notified() => {
                announce();
                tokio::time::sleep(CONNECTIONS_CHANGE_COALESCE).await;
            }
            () = tokio::time::sleep(until_deadline.unwrap_or(Duration::MAX)), if until_deadline.is_some() => {
                if let Err(error) = storage.expire_connection_calls() {
                    tracing::warn!(%error, "could not expire connection calls");
                    tokio::time::sleep(EXPIRY_RETRY).await;
                }
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use std::sync::atomic::{AtomicUsize, Ordering};
    use std::sync::Arc;

    use super::super::store::tests::{call, fixture};
    use super::*;

    fn spawn_relay(storage: Arc<Storage>) -> (Arc<AtomicUsize>, tokio::task::JoinHandle<()>) {
        let announced = Arc::new(AtomicUsize::new(0));
        let counter = announced.clone();
        let task = tokio::spawn(async move {
            relay_connection_changes(&storage, || {
                counter.fetch_add(1, Ordering::SeqCst);
            })
            .await;
        });
        (announced, task)
    }

    async fn settle() {
        tokio::time::sleep(CONNECTIONS_CHANGE_COALESCE * 2).await;
    }

    #[tokio::test]
    async fn a_burst_of_call_writes_is_announced_once() {
        let (storage, connection, session) = fixture();
        let storage = Arc::new(storage);
        let (announced, task) = spawn_relay(storage.clone());
        settle().await;
        let before = announced.load(Ordering::SeqCst);
        for _ in 0..5 {
            storage
                .create_connection_call(call(&connection, session, "pending"))
                .unwrap();
        }
        settle().await;
        assert_eq!(announced.load(Ordering::SeqCst) - before, 1);
        task.abort();
    }

    #[tokio::test]
    async fn writes_to_other_tables_are_not_announced() {
        let (storage, _, _) = fixture();
        let storage = Arc::new(storage);
        let (announced, task) = spawn_relay(storage.clone());
        settle().await;
        let before = announced.load(Ordering::SeqCst);
        storage.create_bucket("unrelated").unwrap();
        settle().await;
        assert_eq!(announced.load(Ordering::SeqCst), before);
        task.abort();
    }

    #[tokio::test]
    async fn an_approval_that_runs_out_is_expired_and_announced() {
        let (storage, connection, session) = fixture();
        let mut pending = call(&connection, session, "pending");
        pending.expires_at = crate::storage::now_unix_ms() + 800;
        let (pending, _) = storage.create_connection_call(pending).unwrap();
        let storage = Arc::new(storage);
        let (announced, task) = spawn_relay(storage.clone());
        settle().await;
        let before = announced.load(Ordering::SeqCst);
        assert_eq!(
            storage.connection_call(&pending.id).unwrap().status,
            "pending"
        );
        tokio::time::sleep(Duration::from_millis(600)).await;
        settle().await;
        assert_eq!(
            storage.connection_call(&pending.id).unwrap().status,
            "expired"
        );
        assert!(announced.load(Ordering::SeqCst) > before);
        task.abort();
    }
}
