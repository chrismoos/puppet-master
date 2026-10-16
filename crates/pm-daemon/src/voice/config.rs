//! Where the voice agent reasons and what it speaks with, resolved from
//! settings.
//!
//! The reasoning endpoint comes from a model profile, so voice shares
//! the credential store and the endpoint configuration that already
//! exist rather than introducing a second one.

use pm_protocol::domain::ModelDialect;

use super::agent::VoiceError;
use super::model::{ModelEndpoint, DEFAULT_MODEL, PROFILE_SETTING};
use super::tts;
use crate::daemon::Daemon;

/// What to render speech with. Absent when nothing is configured, in
/// which case answers still come back as text.
pub struct SpeechConfig {
    pub provider: String,
    pub voice: String,
    pub base_url: String,
    pub api_key: String,
    pub format: String,
    pub model: String,
}

pub struct VoiceConfig {
    pub model: ModelEndpoint,
    pub speech: Option<SpeechConfig>,
}

pub fn resolve(daemon: &Daemon) -> Result<VoiceConfig, VoiceError> {
    Ok(VoiceConfig {
        model: model_endpoint(daemon)?,
        speech: speech_config(daemon),
    })
}

/// The endpoint named by the voice model profile. A profile is named by
/// id or by name, because an operator types a name and a script keeps
/// an id.
fn model_endpoint(daemon: &Daemon) -> Result<ModelEndpoint, VoiceError> {
    let named = daemon.setting(PROFILE_SETTING).unwrap_or_default();
    let named = named.trim();
    if named.is_empty() {
        return Err(VoiceError::NotConfigured(format!(
            "no model profile is set for the voice agent. Set {PROFILE_SETTING} to one"
        )));
    }
    let profiles = daemon
        .storage
        .list_model_profiles()
        .map_err(|e| VoiceError::NotConfigured(e.to_string()))?;
    let profile = profiles
        .iter()
        .find(|profile| {
            named.parse::<u64>().is_ok_and(|id| id == profile.id)
                || profile.name.eq_ignore_ascii_case(named)
        })
        .ok_or_else(|| {
            VoiceError::NotConfigured(format!("no model profile is called {named:?}"))
        })?;
    let entry = profile
        .endpoints
        .iter()
        .find(|entry| entry.dialect == ModelDialect::AnthropicMessages)
        .ok_or_else(|| {
            VoiceError::NotConfigured(format!(
                "the model profile {:?} has no {} endpoint for the voice agent to use",
                profile.name,
                ModelDialect::AnthropicMessages.as_str()
            ))
        })?;
    let api_key = daemon
        .storage
        .model_profile_key_ciphertext(profile.id)
        .ok()
        .flatten()
        .and_then(|sealed| crate::secrets::open_secret(&daemon.installation_secret(), &sealed))
        .unwrap_or_default();
    Ok(ModelEndpoint {
        base_url: entry.base_url.clone(),
        api_key,
        model: if entry.model.trim().is_empty() {
            DEFAULT_MODEL.to_string()
        } else {
            entry.model.clone()
        },
    })
}

/// Where to reach the speech provider, and with what.
///
/// Separate from [`speech_config`] because asking a provider what it can
/// speak with needs no voice chosen: requiring one would mean picking a
/// voice before you could see what there is to pick.
pub fn provider_account(daemon: &Daemon) -> Option<(String, String, String)> {
    let provider = daemon
        .setting(tts::PROVIDER_SETTING)
        .unwrap_or_else(|| "elevenlabs".to_string());
    let base_url = daemon.setting(tts::BASE_URL_SETTING).unwrap_or_default();
    if provider.trim().is_empty() || base_url.trim().is_empty() {
        return None;
    }
    Some((
        provider.trim().to_string(),
        base_url.trim().to_string(),
        daemon
            .secret_setting(tts::API_KEY_SETTING)
            .unwrap_or_default(),
    ))
}

/// Speech is optional on purpose: a controller with no provider
/// configured still answers, in text, which is what a client reading
/// along shows anyway.
fn speech_config(daemon: &Daemon) -> Option<SpeechConfig> {
    let provider = daemon
        .setting(tts::PROVIDER_SETTING)
        .unwrap_or_else(|| "elevenlabs".to_string());
    let base_url = daemon.setting(tts::BASE_URL_SETTING).unwrap_or_default();
    let voice = daemon.setting(tts::VOICE_SETTING).unwrap_or_default();
    if provider.trim().is_empty() || base_url.trim().is_empty() || voice.trim().is_empty() {
        return None;
    }
    Some(SpeechConfig {
        provider: provider.trim().to_string(),
        voice: voice.trim().to_string(),
        base_url: base_url.trim().to_string(),
        api_key: daemon
            .secret_setting(tts::API_KEY_SETTING)
            .unwrap_or_default(),
        format: daemon.setting(tts::FORMAT_SETTING).unwrap_or_default(),
        model: daemon.setting(tts::MODEL_SETTING).unwrap_or_default(),
    })
}
