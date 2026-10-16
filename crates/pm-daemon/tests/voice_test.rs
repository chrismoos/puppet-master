//! The voice loop end to end, against scripted endpoints rather than a
//! provider account.
//!
//! A real provider cannot be asked to force a particular tool call, to
//! stall, or to fail partway through a stream, and it cannot be reached
//! without a credential. A scripted one does all of that in
//! milliseconds, which is why the base URLs are settings.

mod support;

use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};

use axum::extract::Path;
use axum::response::IntoResponse;
use axum::routing::post;
use axum::{Json, Router};
use pm_protocol::domain::{ModelDialect, ServerMsg};
use serde_json::{json, Value};
use support::{daemon_env, spawn_test_session, TestEnv};
use tokio::sync::mpsc;

/// What the scripted model answers, in order. Each entry is one
/// `content` array, so a test can force a tool call and then a reply.
#[derive(Clone, Default)]
struct Script {
    answers: Arc<Mutex<Vec<Value>>>,
    seen: Arc<Mutex<Vec<Value>>>,
    calls: Arc<AtomicUsize>,
}

impl Script {
    fn new(answers: Vec<Value>) -> Self {
        Self {
            answers: Arc::new(Mutex::new(answers)),
            seen: Arc::new(Mutex::new(Vec::new())),
            calls: Arc::new(AtomicUsize::new(0)),
        }
    }

    /// Every request body the model was sent, so a test can assert on
    /// what the agent was told rather than only on what it said.
    fn requests(&self) -> Vec<Value> {
        self.seen.lock().unwrap().clone()
    }
}

async fn model_server(script: Script) -> String {
    let router = Router::new().route(
        "/v1/messages",
        post(move |Json(body): Json<Value>| {
            let script = script.clone();
            async move {
                script.seen.lock().unwrap().push(body);
                script.calls.fetch_add(1, Ordering::SeqCst);
                let next = {
                    let mut answers = script.answers.lock().unwrap();
                    if answers.is_empty() {
                        json!([{ "type": "text", "text": "Nothing else to say." }])
                    } else {
                        answers.remove(0)
                    }
                };
                Json(json!({ "content": next, "stop_reason": "end_turn" }))
            }
        }),
    );
    let listener = tokio::net::TcpListener::bind((std::net::Ipv4Addr::LOCALHOST, 0))
        .await
        .unwrap();
    let base = format!("http://{}", listener.local_addr().unwrap());
    tokio::spawn(async move {
        axum::serve(listener, router).await.unwrap();
    });
    base
}

/// A speech endpoint that answers with recognisable bytes, so a test can
/// tell rendered audio from anything else on the stream.
async fn speech_server() -> (String, Arc<AtomicUsize>) {
    let rendered = Arc::new(AtomicUsize::new(0));
    let count = rendered.clone();
    let router = Router::new().route(
        "/v1/text-to-speech/{voice}/stream",
        post(move |Path(voice): Path<String>, Json(body): Json<Value>| {
            let count = count.clone();
            async move {
                count.fetch_add(1, Ordering::SeqCst);
                let said = body["text"].as_str().unwrap_or_default();
                assert!(!voice.is_empty(), "the voice is named in the path");
                // Two frames, so a test sees streaming rather than
                // one blob.
                let audio: Vec<u8> = format!("<<{said}>>").into_bytes();
                audio.into_response()
            }
        }),
    );
    let listener = tokio::net::TcpListener::bind((std::net::Ipv4Addr::LOCALHOST, 0))
        .await
        .unwrap();
    let base = format!("http://{}", listener.local_addr().unwrap());
    tokio::spawn(async move {
        axum::serve(listener, router).await.unwrap();
    });
    (base, rendered)
}

fn configure_model(env: &TestEnv, base_url: &str) {
    let profile = env
        .daemon
        .create_model_profile("voice", Some("placeholder-key"))
        .unwrap();
    env.daemon
        .set_model_profile_endpoint(
            profile,
            ModelDialect::AnthropicMessages,
            "scripted-model",
            base_url,
            "",
        )
        .unwrap();
    env.daemon
        .set_setting(pm_daemon::voice::model::PROFILE_SETTING, Some("voice"))
        .unwrap();
}

fn configure_speech(env: &TestEnv, base_url: &str) {
    use pm_daemon::voice::tts;
    env.daemon
        .set_setting(tts::BASE_URL_SETTING, Some(base_url))
        .unwrap();
    env.daemon
        .set_setting(tts::VOICE_SETTING, Some("a-voice"))
        .unwrap();
    env.daemon
        .set_setting(tts::API_KEY_SETTING, Some("placeholder-key"))
        .unwrap();
}

/// Drives one turn and collects everything it sent back.
async fn turn(env: &TestEnv, text: &str, wants_audio: bool) -> (u64, Vec<ServerMsg>) {
    let (tx, mut rx) = mpsc::channel(64);
    let id = env
        .daemon
        .voice()
        .begin_turn(&env.daemon, None, text.to_string(), wants_audio, tx);
    let mut sent = Vec::new();
    while let Some(msg) = rx.recv().await {
        let last = matches!(&msg, ServerMsg::VoiceDelta(d) if d.done);
        sent.push(msg);
        if last {
            break;
        }
    }
    (id, sent)
}

fn spoken(sent: &[ServerMsg]) -> String {
    sent.iter()
        .filter_map(|msg| match msg {
            ServerMsg::VoiceDelta(d) if !d.text.is_empty() => Some(d.text.clone()),
            _ => None,
        })
        .collect::<Vec<_>>()
        .join(" ")
}

fn audio(sent: &[ServerMsg]) -> Vec<u8> {
    sent.iter()
        .filter_map(|msg| match msg {
            ServerMsg::VoiceDelta(d) if !d.audio.is_empty() => Some(d.audio.to_vec()),
            _ => None,
        })
        .flatten()
        .collect()
}

/// Whether the session's agent process actually exited.
///
/// No server loop runs in these tests, so nothing reconciles a PTY exit
/// into stored session state. The mux's own exit event is the evidence
/// that a kill reached the process, and its absence is the evidence
/// that none was issued.
async fn exited_within(env: &mut TestEnv, session: u64, within: std::time::Duration) -> bool {
    let deadline = tokio::time::Instant::now() + within;
    loop {
        match tokio::time::timeout_at(deadline, env.exit_rx.recv()).await {
            Ok(Some(exit)) if exit.semantic_session_id == session => return true,
            Ok(Some(_)) => continue,
            Ok(None) | Err(_) => return false,
        }
    }
}

fn error_of(sent: &[ServerMsg]) -> String {
    sent.iter()
        .filter_map(|msg| match msg {
            ServerMsg::VoiceDelta(d) if !d.error.is_empty() => Some(d.error.clone()),
            _ => None,
        })
        .collect::<Vec<_>>()
        .join("; ")
}

#[tokio::test]
async fn a_question_is_answered_in_words_and_in_speech() {
    let env = daemon_env();
    let script = Script::new(vec![json!([
        { "type": "text", "text": "Nothing needs you right now." }
    ])]);
    let model = model_server(script.clone()).await;
    let (speech, rendered) = speech_server().await;
    configure_model(&env, &model);
    configure_speech(&env, &speech);

    let (_, sent) = turn(&env, "what needs me", true).await;

    assert_eq!(spoken(&sent), "Nothing needs you right now.");
    assert!(
        String::from_utf8_lossy(&audio(&sent)).contains("Nothing needs you right now."),
        "the words were rendered as speech"
    );
    assert_eq!(rendered.load(Ordering::SeqCst), 1);
    let codec = sent.iter().any(|msg| match msg {
        ServerMsg::VoiceDelta(d) => d.audio_codec == "pcm_s16le" && d.audio_sample_rate == 24_000,
        _ => false,
    });
    assert!(codec, "the codec is named before the frames it describes");
}

/// The digest is what the agent knows. A session waiting on the user has
/// to be in it, or the agent cannot answer the one question that matters
/// most.
#[tokio::test]
async fn the_agent_is_told_what_is_waiting_on_the_user() {
    let env = daemon_env();
    let _session = spawn_test_session(&env, "subject");
    let script = Script::new(vec![json!([{ "type": "text", "text": "One thing." }])]);
    let model = model_server(script.clone()).await;
    configure_model(&env, &model);

    let (_, _sent) = turn(&env, "what needs me", false).await;

    let request = script
        .requests()
        .first()
        .cloned()
        .expect("the model was called");
    let system = request["system"].as_str().unwrap_or_default().to_string();
    assert!(
        system.contains("What is happening right now"),
        "the digest rides the system prompt: {system}"
    );
    assert!(
        system.contains("write for the ear") || system.contains("Write for the ear"),
        "the speaking rules are given: {system}"
    );
    assert!(
        !system.contains("terminal output"),
        "terminal output is never offered to be read aloud"
    );
}

/// Speech is optional. Without a provider configured the answer still
/// comes back as words, which is what a client reading along shows.
#[tokio::test]
async fn an_answer_still_arrives_when_nothing_can_speak_it() {
    let env = daemon_env();
    let script = Script::new(vec![json!([{ "type": "text", "text": "Still here." }])]);
    let model = model_server(script).await;
    configure_model(&env, &model);

    let (_, sent) = turn(&env, "are you there", true).await;

    assert_eq!(spoken(&sent), "Still here.");
    assert!(audio(&sent).is_empty(), "nothing was configured to speak");
    assert!(error_of(&sent).is_empty(), "that is not a failure");
}

/// Stopping a session is refused until the user has agreed out loud. The
/// refusal has to hand back something the agent can say.
#[tokio::test]
async fn stopping_a_session_is_refused_until_the_user_confirms() {
    let mut env = daemon_env();
    let session = spawn_test_session(&env, "subject");
    let script = Script::new(vec![
        json!([{
            "type": "tool_use", "id": "c1", "name": "kill_session",
            "input": { "session": session }
        }]),
        json!([{ "type": "text", "text": "Do you want me to stop it?" }]),
    ]);
    let model = model_server(script.clone()).await;
    configure_model(&env, &model);

    let (_, sent) = turn(&env, "stop that one", false).await;

    assert_eq!(spoken(&sent), "Do you want me to stop it?");
    assert!(
        !exited_within(&mut env, session, std::time::Duration::from_millis(600)).await,
        "no kill was issued, so the agent is still running"
    );
    let results = script.requests();
    let handed_back = serde_json::to_string(&results).unwrap();
    assert!(
        handed_back.contains("needs_confirmation"),
        "the tool told the agent to ask first: {handed_back}"
    );
}

/// The model cannot confirm to itself: a token minted while answering
/// one utterance is not spendable until the user speaks again.
#[tokio::test]
async fn a_confirmation_cannot_be_spent_on_the_same_utterance() {
    let mut env = daemon_env();
    let session = spawn_test_session(&env, "subject");
    let script = Script::new(vec![
        json!([{
            "type": "tool_use", "id": "c1", "name": "kill_session",
            "input": { "session": session }
        }]),
        // The model tries to spend the token it was just handed.
        json!([{
            "type": "tool_use", "id": "c2", "name": "kill_session",
            "input": { "session": session, "confirm_token": "confirm-1" }
        }]),
        json!([{ "type": "text", "text": "I need you to confirm that." }]),
    ]);
    let model = model_server(script).await;
    configure_model(&env, &model);

    let (_, _sent) = turn(&env, "stop it", false).await;

    assert!(
        !exited_within(&mut env, session, std::time::Duration::from_millis(600)).await,
        "the session must survive the model confirming to itself"
    );
}

/// With the user's agreement in between, the same token acts.
#[tokio::test]
async fn a_confirmed_stop_takes_effect_on_the_next_utterance() {
    let mut env = daemon_env();
    let session = spawn_test_session(&env, "subject");
    let script = Script::new(vec![
        json!([{
            "type": "tool_use", "id": "c1", "name": "kill_session",
            "input": { "session": session }
        }]),
        json!([{ "type": "text", "text": "Shall I stop it?" }]),
        json!([{
            "type": "tool_use", "id": "c2", "name": "kill_session",
            "input": { "session": session, "confirm_token": "confirm-1" }
        }]),
        json!([{ "type": "text", "text": "Stopped." }]),
    ]);
    let model = model_server(script.clone()).await;
    configure_model(&env, &model);

    let (tx, mut rx) = mpsc::channel(64);
    let conversation =
        env.daemon
            .voice()
            .begin_turn(&env.daemon, None, "stop it".into(), false, tx.clone());
    while let Some(msg) = rx.recv().await {
        if matches!(&msg, ServerMsg::VoiceDelta(d) if d.done) {
            break;
        }
    }
    // The user agrees, which is the later utterance the token needs.
    env.daemon
        .voice()
        .begin_turn(&env.daemon, Some(conversation), "yes".into(), false, tx);
    while let Some(msg) = rx.recv().await {
        if matches!(&msg, ServerMsg::VoiceDelta(d) if d.done) {
            break;
        }
    }

    assert!(
        exited_within(&mut env, session, std::time::Duration::from_secs(10)).await,
        "the confirmed stop reached the agent"
    );
}

/// Changing anything needs a project chosen first, because guessing
/// across several starts work in the wrong repository.
#[tokio::test]
async fn spawning_without_a_focus_asks_which_project() {
    let env = daemon_env();
    let script = Script::new(vec![
        json!([{
            "type": "tool_use", "id": "s1", "name": "spawn_session",
            "input": { "prompt": "run the suite" }
        }]),
        json!([{ "type": "text", "text": "Which project should I start that in?" }]),
    ]);
    let model = model_server(script.clone()).await;
    configure_model(&env, &model);

    let (_, sent) = turn(&env, "start a session to run the suite", false).await;

    assert_eq!(spoken(&sent), "Which project should I start that in?");
    let handed_back = serde_json::to_string(&script.requests()).unwrap();
    assert!(
        handed_back.contains("no project is focused"),
        "the refusal names what is missing: {handed_back}"
    );
}

/// A failure the user needs to know about arrives as an error on the
/// stream rather than as silence.
#[tokio::test]
async fn a_model_that_cannot_be_reached_is_reported_not_swallowed() {
    let env = daemon_env();
    let profile = env
        .daemon
        .create_model_profile("voice", Some("placeholder-key"))
        .unwrap();
    env.daemon
        .set_model_profile_endpoint(
            profile,
            ModelDialect::AnthropicMessages,
            "scripted-model",
            // Nothing listens here.
            "http://127.0.0.1:1",
            "",
        )
        .unwrap();
    env.daemon
        .set_setting(pm_daemon::voice::model::PROFILE_SETTING, Some("voice"))
        .unwrap();

    let (_, sent) = turn(&env, "what needs me", false).await;

    assert!(
        error_of(&sent).contains("could not reach the model"),
        "the user is told: {:?}",
        error_of(&sent)
    );
}

/// A profile whose endpoint has no base url is the easy mistake to make,
/// because an empty one is how an agent CLI is told to use its own
/// account. The daemon has no account, so the error has to say so.
#[tokio::test]
async fn a_profile_without_a_base_url_says_to_set_one() {
    let env = daemon_env();
    let profile = env
        .daemon
        .create_model_profile("voice", Some("placeholder-key"))
        .unwrap();
    env.daemon
        .set_model_profile_endpoint(
            profile,
            ModelDialect::AnthropicMessages,
            "scripted-model",
            "",
            "",
        )
        .unwrap();
    env.daemon
        .set_setting(pm_daemon::voice::model::PROFILE_SETTING, Some("voice"))
        .unwrap();

    let (_, sent) = turn(&env, "what needs me", false).await;
    let error = error_of(&sent);
    assert!(
        error.contains("base url"),
        "the error names what to set: {error}"
    );
}

/// With no profile set at all the failure has to say what an operator
/// should do, because no retry fixes it.
#[tokio::test]
async fn an_unconfigured_controller_says_what_is_missing() {
    let env = daemon_env();
    let (_, sent) = turn(&env, "what needs me", false).await;
    let error = error_of(&sent);
    assert!(
        error.contains("voice.model.profile"),
        "the setting to fix is named: {error}"
    );
}
