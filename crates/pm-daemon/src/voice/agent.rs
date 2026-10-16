//! One voice conversation at a time, from an utterance to spoken words.
//!
//! The loop runs here rather than on each client so that the tools act
//! in-process against real state, the credentials stay sealed in one
//! place, and every client sounds the same. A client sends text and
//! plays what comes back.

use std::collections::HashMap;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex};

use pm_protocol::domain::{ServerMsg, VoiceDelta};
use serde_json::{json, Value};
use tokio::sync::mpsc;
use tracing::{debug, info, warn};

use super::config::{self, VoiceConfig};
use super::digest::digest;
use super::model::{sentences, ModelClient, ModelError};
use super::tools::{self, VoiceState};
use super::tts::{SpeechCache, TtsRegistry};
use crate::daemon::Daemon;

/// How many tool rounds one utterance may take before the agent has to
/// say something. A loop that cannot finish in this many is stuck, and
/// silence is worse than an admission.
const MAX_TOOL_ROUNDS: usize = 6;

/// Speech kept for reuse. The agent repeats itself heavily, and
/// rendering is billed per character.
const SPEECH_CACHE_BYTES: usize = 8 * 1024 * 1024;

/// How the agent is told to behave. The rules it cannot be trusted to
/// follow are enforced in the tools: this is about how it speaks.
const PERSONA: &str = "\
You are Puppet Master's voice. The user is talking to you out loud, away from their \
screen, and your answer is read aloud to them.

Write for the ear, never for the eye. One sentence is usually right and two is the \
most, unless the user asks for more. Use contractions. Say numbers as words. Never say \
a file path, a commit hash, a URL, a bracket, or a markdown character. Never read out a \
list of more than three things: say how many there are and offer the ones that matter.

Answer the question and stop. Do not say what you just did, do not repeat the question \
back, and do not explain how any of this works: the user asked about their work, not \
about you. \"I asked it and it is queued\" is three facts they did not ask for where \
\"I have asked it\" is the answer.

Refer to a session by what it is doing, not by its number. Refer to a board item by its \
title. Item numbers repeat across buckets, so a number said out loud is ambiguous.

You can see everything, everywhere. Starting new work needs a project in focus, because \
there is nowhere to put it otherwise, and if it is ambiguous you ask which one rather \
than guessing: guessing starts work in the wrong repository. Anything aimed at a \
session you already have the number for needs no focus.

Anything that disturbs a running session has to be confirmed by the user first. When a \
tool tells you to ask, say exactly what it gives you and wait for their answer.

Most agents cannot be interrupted part way through a turn, so a message to one that is \
working waits for it to finish. Say so only when the result says it waits, because then \
the thing the user asked for is not happening yet. When it starts straight away there is \
nothing to explain, so do not mention delivery at all.

You are told what each session reports about itself. You cannot see their terminals, so \
do not pretend to. If a session has not reported in a while, say when it last said \
anything rather than repeating a stale line as though it were current.

What you are told lists only a few sessions per state. When the user names work you \
cannot see in it, look it up rather than saying you cannot find it.

Pass an agent the user's own words rather than your version of them: leave the message \
text out and what they said is forwarded exactly. Their question is for the agent, not \
for you to improve.

When the user wants to know what an agent says back, ask for a reply. Say nothing else \
that turn: the agent's own answer is read out to them when it arrives, and anything you \
add is talking over it.";

struct Conversation {
    state: VoiceState,
    /// The exchange so far, in the provider's own shape, so tool
    /// results append without a translation layer.
    messages: Vec<Value>,
    /// The turn in flight, so a later interruption can abandon it.
    running: Option<tokio::task::JoinHandle<()>>,
    /// Where to send an answer that arrives after the turn that asked
    /// for it has finished. Live only: a reply for a client that has
    /// gone is recorded on the message and not spoken to anyone.
    out: Option<mpsc::Sender<ServerMsg>>,
    wants_audio: bool,
}

impl Conversation {
    fn new() -> Self {
        Self {
            state: VoiceState::default(),
            messages: Vec::new(),
            running: None,
            out: None,
            wants_audio: false,
        }
    }
}

/// A message whose answer is to be read out, and whether the agent has
/// already been asked once to say it in a speakable way.
#[derive(Clone, Copy)]
pub struct Awaited {
    pub conversation_id: u64,
    pub asked_again: bool,
}

pub struct VoiceAgent {
    conversations: Mutex<HashMap<u64, Conversation>>,
    /// Which conversation asked to hear the answer to which message.
    awaited: Mutex<HashMap<u64, Awaited>>,
    next_id: AtomicU64,
    model: ModelClient,
    tts: TtsRegistry,
    cache: Option<SpeechCache>,
}

impl Default for VoiceAgent {
    /// The same agent `new` builds. A derived default would differ from
    /// it by having no speech cache, which is not a second kind of voice
    /// agent anybody wants.
    fn default() -> Self {
        Self::new()
    }
}

impl VoiceAgent {
    pub fn new() -> Self {
        Self {
            conversations: Mutex::new(HashMap::new()),
            awaited: Mutex::new(HashMap::new()),
            next_id: AtomicU64::new(1),
            model: ModelClient::default(),
            tts: TtsRegistry::default(),
            cache: Some(SpeechCache::new(SPEECH_CACHE_BYTES)),
        }
    }

    /// Abandons the turn in flight. Dropping its task drops the speech
    /// stream, which closes the provider connection, so an
    /// interruption also stops paying for the rest of the synthesis.
    pub fn cancel(&self, conversation_id: u64) {
        let mut conversations = self.conversations.lock().unwrap();
        if let Some(conversation) = conversations.get_mut(&conversation_id) {
            if let Some(running) = conversation.running.take() {
                running.abort();
                debug!(conversation = conversation_id, "voice turn abandoned");
            }
        }
    }

    /// Starts a turn, answering on `out` as it goes. Returns the
    /// conversation the turn belongs to, which a new conversation needs
    /// so its next utterance continues this one.
    pub fn begin_turn(
        self: &Arc<Self>,
        daemon: &Arc<Daemon>,
        conversation_id: Option<u64>,
        text: String,
        wants_audio: bool,
        out: mpsc::Sender<ServerMsg>,
    ) -> u64 {
        let id = conversation_id.unwrap_or_else(|| self.next_id.fetch_add(1, Ordering::Relaxed));
        let agent = Arc::clone(self);
        let daemon = Arc::clone(daemon);
        let out_for_later = out.clone();
        let running = tokio::spawn(async move {
            if let Err(error) = agent.run_turn(&daemon, id, &text, wants_audio, &out).await {
                warn!(conversation = id, %error, "voice turn failed");
                let _ = out
                    .send(ServerMsg::VoiceDelta(VoiceDelta {
                        conversation_id: id,
                        done: true,
                        error: error.to_string(),
                        ..VoiceDelta::default()
                    }))
                    .await;
            }
        });
        let mut conversations = self.conversations.lock().unwrap();
        let conversation = conversations.entry(id).or_insert_with(Conversation::new);
        conversation.out = Some(out_for_later);
        conversation.wants_audio = wants_audio;
        if let Some(previous) = conversation.running.replace(running) {
            // A new utterance supersedes one still being answered, which
            // is what barging in over the agent means.
            previous.abort();
        }
        id
    }

    async fn run_turn(
        &self,
        daemon: &Arc<Daemon>,
        id: u64,
        said: &str,
        wants_audio: bool,
        out: &mpsc::Sender<ServerMsg>,
    ) -> Result<(), VoiceError> {
        let config = config::resolve(daemon)?;
        let snapshot = daemon.state_snapshot();
        let now = crate::daemon::now_unix_ms();

        let (mut messages, focus) = {
            let mut conversations = self.conversations.lock().unwrap();
            let conversation = conversations.entry(id).or_insert_with(Conversation::new);
            // The turn counter is what makes a confirmation need the
            // user's voice: a token minted on an earlier turn can be
            // spent, one minted on this turn cannot.
            conversation.state.turn += 1;
            conversation.state.said = said.to_string();
            conversation.messages.push(json!({
                "role": "user",
                "content": said,
            }));
            (conversation.messages.clone(), conversation.state.focus)
        };

        let system = format!(
            "{PERSONA}\n\n--- What is happening right now ---\n{}",
            digest(&snapshot, now, &focus)
        );

        let catalog = tools::catalog();
        let mut answer = String::new();
        // A turn that ends by waiting on an agent says nothing: the user
        // hears the agent's own answer when it comes, not a narration of
        // the asking.
        let mut asked_for_a_reply = false;
        for round in 0..MAX_TOOL_ROUNDS {
            let turn = self
                .model
                .turn(&config.model, &system, &messages, &catalog)
                .await?;
            messages.push(json!({ "role": "assistant", "content": turn.content }));
            if turn.tool_calls.is_empty() {
                answer = turn.text;
                break;
            }
            // Words that come with a tool call are the agent narrating
            // what it is about to do, which is worth hearing while it
            // does it.
            if !turn.text.trim().is_empty() {
                self.say(&config, id, &turn.text, wants_audio, out).await?;
            }
            let mut results = Vec::new();
            for call in &turn.tool_calls {
                // Focus and confirmation live with the conversation, so
                // they are taken out, carried through the call, and put
                // back. No lock is held across it.
                let mut state = {
                    let mut conversations = self.conversations.lock().unwrap();
                    let conversation = conversations.entry(id).or_insert_with(Conversation::new);
                    std::mem::take(&mut conversation.state)
                };
                let outcome = tools::call(daemon, &mut state, &snapshot, call).await;
                let awaiting = std::mem::take(&mut state.awaiting_replies);
                {
                    let mut conversations = self.conversations.lock().unwrap();
                    if let Some(conversation) = conversations.get_mut(&id) {
                        conversation.state = state;
                    }
                }
                if !awaiting.is_empty() {
                    asked_for_a_reply = true;
                    let mut awaited = self.awaited.lock().unwrap();
                    for message in awaiting {
                        awaited.insert(
                            message,
                            Awaited {
                                conversation_id: id,
                                asked_again: false,
                            },
                        );
                    }
                }
                let ok = outcome
                    .get("ok")
                    .and_then(serde_json::Value::as_bool)
                    .unwrap_or(false);
                info!(
                    conversation = id,
                    tool = call.name.as_str(),
                    ok,
                    "voice tool call"
                );
                results.push(json!({
                    "type": "tool_result",
                    "tool_use_id": call.id,
                    "content": outcome.to_string(),
                }));
            }
            messages.push(json!({ "role": "user", "content": results }));
            if round + 1 == MAX_TOOL_ROUNDS {
                answer =
                    "I got stuck working that out. Can you say it a different way?".to_string();
            }
        }

        if asked_for_a_reply {
            debug!(
                conversation = id,
                "holding the turn silent until the agent answers"
            );
        } else {
            if answer.trim().is_empty() {
                answer = "I did that.".to_string();
            }
            self.say(&config, id, &answer, wants_audio, out).await?;
        }

        {
            let mut conversations = self.conversations.lock().unwrap();
            if let Some(conversation) = conversations.get_mut(&id) {
                conversation.messages = messages;
                conversation.running = None;
            }
        }
        let _ = out
            .send(ServerMsg::VoiceDelta(VoiceDelta {
                conversation_id: id,
                done: true,
                ..VoiceDelta::default()
            }))
            .await;
        Ok(())
    }

    /// The models the configured provider can speak with, asked of the
    /// provider itself so the credential stays in this process.
    pub async fn speech_models(
        &self,
        daemon: &Daemon,
    ) -> Result<Vec<super::tts::SpeechModel>, VoiceError> {
        let Some((provider_id, base_url, api_key)) = config::provider_account(daemon) else {
            return Err(VoiceError::NotConfigured(
                "no speech provider base url is set, so there is nowhere to ask".into(),
            ));
        };
        let provider = self.tts.get(&provider_id)?;
        Ok(provider
            .models(super::tts::ProviderAccount {
                base_url: &base_url,
                api_key: &api_key,
            })
            .await?)
    }

    /// The speech providers this build can render with, so a client can
    /// offer the real choices rather than a list it keeps in step by
    /// hand.
    pub fn speech_providers(&self) -> Vec<&'static str> {
        self.tts.ids()
    }

    /// Claims the conversation waiting to hear this message's answer.
    /// Taking it means this answer is dealt with once, so a replayed
    /// event does not say the same thing twice.
    pub fn take_awaited(&self, message_id: u64) -> Option<Awaited> {
        self.awaited.lock().unwrap().remove(&message_id)
    }

    /// Waits on a different message for the same conversation, which is
    /// what asking the agent to say it again amounts to.
    pub fn await_instead(&self, message_id: u64, awaited: Awaited) {
        self.awaited.lock().unwrap().insert(message_id, awaited);
    }

    /// Reads out an agent's answer to a message the user sent by voice.
    ///
    /// The answer arrives whenever the agent gets to it, long after the
    /// turn that asked for it, so it is spoken on its own rather than
    /// returned as a tool result: waiting for it inside the turn would
    /// hold the conversation open for as long as the agent takes.
    ///
    /// Spoken exactly as the agent wrote it. Nothing is prepended: by
    /// the time a message is sent the conversation has already settled
    /// which session it went to, so naming it again is noise.
    ///
    /// Returns whether anybody was still listening.
    pub async fn deliver_agent_reply(
        &self,
        conversation_id: u64,
        body: &str,
        config: &VoiceConfig,
    ) -> bool {
        // Live only: a reply for a client that has gone stays on the
        // message row rather than being spoken to nobody.
        let Some((out, wants_audio)) = ({
            let conversations = self.conversations.lock().unwrap();
            conversations
                .get(&conversation_id)
                .and_then(|conversation| {
                    conversation
                        .out
                        .clone()
                        .map(|out| (out, conversation.wants_audio))
                })
        }) else {
            return false;
        };
        let spoken = body.trim();
        info!(
            conversation = conversation_id,
            characters = spoken.chars().count(),
            "speaking an agent's reply"
        );
        if let Err(error) = self
            .say(config, conversation_id, spoken, wants_audio, &out)
            .await
        {
            warn!(conversation = conversation_id, %error, "an agent's reply was not spoken");
            return false;
        }
        let _ = out
            .send(ServerMsg::VoiceDelta(VoiceDelta {
                conversation_id,
                done: true,
                ..VoiceDelta::default()
            }))
            .await;
        true
    }

    /// Sends words, then the speech for them a sentence at a time, so
    /// playback starts before the whole answer is rendered.
    async fn say(
        &self,
        config: &VoiceConfig,
        id: u64,
        text: &str,
        wants_audio: bool,
        out: &mpsc::Sender<ServerMsg>,
    ) -> Result<(), VoiceError> {
        let _ = out
            .send(ServerMsg::VoiceDelta(VoiceDelta {
                conversation_id: id,
                text: text.to_string(),
                ..VoiceDelta::default()
            }))
            .await;
        if !wants_audio {
            return Ok(());
        }
        let Some(speech) = config.speech.as_ref() else {
            // Text still reached the client, which is the honest
            // outcome when nothing is configured to speak with.
            debug!(conversation = id, "no speech provider is configured");
            return Ok(());
        };
        let provider = self.tts.get(&speech.provider)?;
        for sentence in sentences(text) {
            let key = super::tts::SpeechKey {
                provider: &speech.provider,
                voice: &speech.voice,
                format: &speech.format,
                text: &sentence,
            };
            let cached = self.cache.as_ref().and_then(|cache| cache.get(key));
            let mut stream = match cached {
                Some(stream) => stream,
                None => {
                    provider
                        .speak(super::tts::SpeakRequest {
                            text: &sentence,
                            voice: &speech.voice,
                            api_key: &speech.api_key,
                            base_url: &speech.base_url,
                            format: &speech.format,
                            model: &speech.model,
                        })
                        .await?
                }
            };
            let codec = stream.codec.clone();
            let sample_rate = stream.sample_rate;
            // Naming the codec before the first frame is what lets a
            // client know how to play what follows.
            let _ = out
                .send(ServerMsg::VoiceDelta(VoiceDelta {
                    conversation_id: id,
                    audio_codec: codec.clone(),
                    audio_sample_rate: sample_rate,
                    ..VoiceDelta::default()
                }))
                .await;
            let mut rendered = Vec::new();
            while let Some(frame) = stream.frames.recv().await {
                let frame = frame?;
                rendered.push(frame.clone());
                if out
                    .send(ServerMsg::VoiceDelta(VoiceDelta {
                        conversation_id: id,
                        audio: frame,
                        ..VoiceDelta::default()
                    }))
                    .await
                    .is_err()
                {
                    // Nobody is listening any more.
                    return Ok(());
                }
            }
            if let Some(cache) = self.cache.as_ref() {
                cache.put(key, &codec, sample_rate, rendered);
            }
        }
        Ok(())
    }
}

#[derive(Debug, thiserror::Error)]
pub enum VoiceError {
    #[error(transparent)]
    Model(#[from] ModelError),
    #[error(transparent)]
    Speech(#[from] super::tts::TtsError),
    #[error("{0}")]
    NotConfigured(String),
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::voice::model::ModelEndpoint;

    /// No speech provider, so `say` sends the words and stops. The
    /// routing is what these tests are about, not the synthesis.
    fn silent_config() -> VoiceConfig {
        VoiceConfig {
            model: ModelEndpoint {
                base_url: "http://127.0.0.1".into(),
                api_key: "k".into(),
                model: "m".into(),
            },
            speech: None,
        }
    }

    /// Registers a conversation that is listening, as a turn would.
    fn listening(agent: &VoiceAgent, id: u64) -> mpsc::Receiver<ServerMsg> {
        let (out, rx) = mpsc::channel(16);
        let mut conversations = agent.conversations.lock().unwrap();
        let conversation = conversations.entry(id).or_insert_with(Conversation::new);
        conversation.out = Some(out);
        conversation.wants_audio = false;
        rx
    }

    /// The agent's words reach the speaker unchanged. By the time a
    /// message is sent the conversation has settled which session it
    /// went to, so naming it again is noise the user did not ask for.
    #[tokio::test]
    async fn an_answer_is_read_out_exactly_as_the_agent_wrote_it() {
        let agent = VoiceAgent::new();
        let mut rx = listening(&agent, 7);

        assert!(
            agent
                .deliver_agent_reply(7, "  Checking in phone 1000 now.  ", &silent_config())
                .await
        );

        let said = match rx.recv().await {
            Some(ServerMsg::VoiceDelta(delta)) => delta.text,
            other => panic!("expected words, got {other:?}"),
        };
        assert_eq!(said, "Checking in phone 1000 now.");
    }

    /// Live only: the conversation that asked has gone, so the answer
    /// stays on the message row rather than being spoken to nobody.
    #[tokio::test]
    async fn a_reply_for_a_client_that_has_gone_is_not_spoken() {
        let agent = VoiceAgent::new();
        assert!(
            !agent
                .deliver_agent_reply(7, "Done.", &silent_config())
                .await
        );
    }

    /// Claiming the wait is what makes a replayed event harmless, and
    /// it carries the conversation the answer belongs to.
    #[tokio::test]
    async fn a_waiting_message_is_claimed_once() {
        let agent = VoiceAgent::new();
        agent.await_instead(
            42,
            Awaited {
                conversation_id: 7,
                asked_again: false,
            },
        );

        let claimed = agent.take_awaited(42).expect("the wait was registered");
        assert_eq!(claimed.conversation_id, 7);
        assert!(!claimed.asked_again);
        assert!(
            agent.take_awaited(42).is_none(),
            "a replayed answer is not spoken twice"
        );
    }

    /// Asking the agent to say it again waits on the new message for
    /// the same conversation, and marks it so a second unspeakable
    /// answer is read out rather than asked for again forever.
    #[tokio::test]
    async fn asking_again_waits_on_the_new_message_only_once() {
        let agent = VoiceAgent::new();
        agent.await_instead(
            99,
            Awaited {
                conversation_id: 7,
                asked_again: true,
            },
        );
        let claimed = agent
            .take_awaited(99)
            .expect("the second wait is registered");
        assert_eq!(claimed.conversation_id, 7);
        assert!(claimed.asked_again, "the corrective round is spent");
    }
}
