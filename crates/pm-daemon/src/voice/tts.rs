//! Turning an answer into speech, behind a provider boundary.
//!
//! The daemon renders speech rather than each client doing it, for three
//! reasons: the credential stays in one sealed place, every client
//! sounds the same, and the phrases this agent repeats can be cached
//! once for all of them. A client is a microphone and a speaker.

use std::collections::{HashMap, VecDeque};
use std::sync::Mutex;

use bytes::Bytes;
use tokio::sync::mpsc;

/// Names the provider to render with.
pub const PROVIDER_SETTING: &str = "voice.tts.provider";
/// The provider's own identifier for the voice to speak in.
pub const VOICE_SETTING: &str = "voice.tts.voice";
/// Where the provider's API lives. No default ships: an operator points
/// this at their provider, or at a stand-in when testing.
pub const BASE_URL_SETTING: &str = "voice.tts.base_url";
/// Sealed at rest and never read back, like the other credentials.
pub const API_KEY_SETTING: &str = "voice.tts.api_key";
/// What the provider should encode to. Providers name formats their own
/// way, so this is passed through rather than interpreted.
pub const FORMAT_SETTING: &str = "voice.tts.format";
/// Which of the provider's models renders the speech.
pub const MODEL_SETTING: &str = "voice.tts.model";

/// Signed 16-bit mono at 24 kHz, which every client can play without a
/// decoder. A five-second answer is about 240 KB, so a provider that
/// offers a compressed format is worth selecting where the listener is
/// on a slow link.
pub const DEFAULT_FORMAT: &str = "pcm_24000";
pub const PCM16_CODEC: &str = "pcm_s16le";
pub const DEFAULT_SAMPLE_RATE: u32 = 24_000;

/// How many frames a provider may run ahead of the client reading them.
/// Back-pressure here is what stops a fast provider buffering a whole
/// answer in memory.
const FRAME_CHANNEL_DEPTH: usize = 16;

#[derive(Debug, thiserror::Error)]
pub enum TtsError {
    /// Nothing is configured to render with, which is an operator
    /// setting rather than something a retry fixes.
    #[error("{0}")]
    NotConfigured(String),
    #[error("no text-to-speech provider named {0:?}")]
    UnknownProvider(String),
    /// The provider was reached and refused, or failed mid-stream.
    #[error("{0}")]
    Provider(String),
    #[error("could not reach the text-to-speech provider: {0}")]
    Unreachable(String),
}

/// What to say, and what to say it with.
pub struct SpeakRequest<'a> {
    pub text: &'a str,
    pub voice: &'a str,
    pub api_key: &'a str,
    pub base_url: &'a str,
    pub format: &'a str,
    /// Empty takes the provider's own low-latency default.
    pub model: &'a str,
}

/// One rendered frame, or the reason rendering stopped.
pub type Frame = Result<Bytes, TtsError>;
pub type FrameSender = mpsc::Sender<Frame>;
pub type FrameReceiver = mpsc::Receiver<Frame>;

/// Speech arriving as it is rendered.
///
/// Dropping `frames` is how a listener stops the answer: the provider's
/// send fails, it stops reading, and the connection closes, so an
/// interruption also stops paying for the rest of the synthesis.
pub struct SpeechStream {
    /// Names what the frames are, sent to the client before the first of
    /// them so it knows how to play them.
    pub codec: String,
    pub sample_rate: u32,
    pub frames: FrameReceiver,
}

impl SpeechStream {
    /// A stream whose frames are already known, which is what a cache
    /// hit and a stand-in provider both hand back.
    pub fn ready(codec: &str, sample_rate: u32, frames: Vec<Bytes>) -> Self {
        let (tx, rx) = mpsc::channel(frames.len().max(1));
        for frame in frames {
            // The channel was sized for exactly these frames.
            let _ = tx.try_send(Ok(frame));
        }
        Self {
            codec: codec.to_string(),
            sample_rate,
            frames: rx,
        }
    }

    /// Collects the whole answer. For a caller that is caching it or
    /// asserting on it rather than playing it as it arrives.
    pub async fn collect(mut self) -> Result<Vec<Bytes>, TtsError> {
        let mut frames = Vec::new();
        while let Some(frame) = self.frames.recv().await {
            frames.push(frame?);
        }
        Ok(frames)
    }
}

/// Creates the channel a provider renders into, sized so the provider
/// cannot run far ahead of the listener.
pub fn frame_channel() -> (FrameSender, FrameReceiver) {
    mpsc::channel(FRAME_CHANNEL_DEPTH)
}

/// One model a provider can speak with, for a client to choose from.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize)]
pub struct SpeechModel {
    pub id: String,
    /// What the provider calls it, which is what a person recognizes.
    pub name: String,
}

/// What a provider needs to answer for itself, which is the credential
/// and where to reach it.
pub struct ProviderAccount<'a> {
    pub base_url: &'a str,
    pub api_key: &'a str,
}

/// One text-to-speech provider.
#[async_trait::async_trait]
pub trait TtsProvider: Send + Sync {
    /// What a setting names this provider by.
    fn id(&self) -> &'static str;

    /// The format to ask for when no setting chooses one.
    fn default_format(&self) -> &'static str {
        DEFAULT_FORMAT
    }

    /// Whether this provider needs a credential. A stand-in does not,
    /// and refusing to speak without one would make it untestable.
    fn needs_api_key(&self) -> bool {
        true
    }

    async fn speak(&self, request: SpeakRequest<'_>) -> Result<SpeechStream, TtsError>;

    /// The models this provider can speak with. Empty is not a failure:
    /// a provider that cannot say leaves the setting free text, which is
    /// also what keeps a model newer than this build selectable.
    async fn models(&self, _account: ProviderAccount<'_>) -> Result<Vec<SpeechModel>, TtsError> {
        Ok(Vec::new())
    }
}

/// The providers this daemon can render with, looked up by the id a
/// setting names. Mirrors how agent adapters are registered, so a second
/// provider is a registration rather than a branch.
pub struct TtsRegistry {
    providers: Vec<Box<dyn TtsProvider>>,
}

impl TtsRegistry {
    pub fn empty() -> Self {
        Self {
            providers: Vec::new(),
        }
    }

    pub fn register(&mut self, provider: Box<dyn TtsProvider>) {
        self.providers.push(provider);
    }

    pub fn get(&self, id: &str) -> Result<&dyn TtsProvider, TtsError> {
        self.providers
            .iter()
            .map(|provider| provider.as_ref())
            .find(|provider| provider.id() == id)
            .ok_or_else(|| TtsError::UnknownProvider(id.to_string()))
    }

    /// Every provider's id, for an operator choosing one.
    pub fn ids(&self) -> Vec<&'static str> {
        self.providers.iter().map(|p| p.id()).collect()
    }
}

impl Default for TtsRegistry {
    fn default() -> Self {
        let mut registry = Self::empty();
        registry.register(Box::new(super::elevenlabs::ElevenLabs::default()));
        registry
    }
}

/// Speech already rendered, so the lines the agent says most often cost
/// nothing after the first time.
///
/// This agent repeats itself heavily: "nothing needs you", "which
/// project?", "asked, I will tell you when it answers", and every
/// session goal it reads out more than once. Rendering is billed per
/// character, so the repetition is worth keeping.
pub struct SpeechCache {
    entries: Mutex<CacheState>,
    max_bytes: usize,
}

#[derive(Default)]
struct CacheState {
    by_key: HashMap<String, CachedSpeech>,
    order: VecDeque<String>,
    bytes: usize,
}

#[derive(Clone)]
struct CachedSpeech {
    codec: String,
    sample_rate: u32,
    frames: Vec<Bytes>,
    bytes: usize,
}

/// Everything that decides how a phrase sounds. Taken together so a
/// voice or format change cannot serve audio rendered for the previous
/// one.
#[derive(Clone, Copy)]
pub struct SpeechKey<'b> {
    pub provider: &'b str,
    pub voice: &'b str,
    pub format: &'b str,
    pub text: &'b str,
}

impl SpeechKey<'_> {
    fn to_cache_key(self) -> String {
        format!(
            "{}\u{1f}{}\u{1f}{}\u{1f}{}",
            self.provider, self.voice, self.format, self.text
        )
    }
}

impl SpeechCache {
    pub fn new(max_bytes: usize) -> Self {
        Self {
            entries: Mutex::new(CacheState::default()),
            max_bytes,
        }
    }

    pub fn get(&self, key: SpeechKey<'_>) -> Option<SpeechStream> {
        let key = key.to_cache_key();
        let entry = self.entries.lock().unwrap().by_key.get(&key).cloned()?;
        Some(SpeechStream::ready(
            &entry.codec,
            entry.sample_rate,
            entry.frames,
        ))
    }

    pub fn put(&self, key: SpeechKey<'_>, codec: &str, sample_rate: u32, frames: Vec<Bytes>) {
        let bytes: usize = frames.iter().map(|f| f.len()).sum();
        if bytes > self.max_bytes {
            return;
        }
        let key = key.to_cache_key();
        let mut state = self.entries.lock().unwrap();
        if state.by_key.contains_key(&key) {
            return;
        }
        while state.bytes + bytes > self.max_bytes {
            let Some(oldest) = state.order.pop_front() else {
                break;
            };
            if let Some(dropped) = state.by_key.remove(&oldest) {
                state.bytes -= dropped.bytes;
            }
        }
        state.order.push_back(key.clone());
        state.bytes += bytes;
        state.by_key.insert(
            key,
            CachedSpeech {
                codec: codec.to_string(),
                sample_rate,
                frames,
                bytes,
            },
        );
    }
}
