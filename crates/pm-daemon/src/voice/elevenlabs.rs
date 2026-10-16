//! The first text-to-speech provider.
//!
//! Nothing here is specific to the voice agent, and nothing elsewhere
//! names this provider: it is reached through `TtsProvider`, so a second
//! one is a registration rather than a branch.

use tracing::{debug, warn};

use super::tts::{
    frame_channel, ProviderAccount, SpeakRequest, SpeechModel, SpeechStream, TtsError, TtsProvider,
    DEFAULT_FORMAT, DEFAULT_SAMPLE_RATE, PCM16_CODEC,
};

/// The low-latency model. Rendering starts before the whole answer is
/// known, so time to the first frame is what the listener feels.
const DEFAULT_MODEL: &str = "eleven_flash_v2_5";

#[derive(Default)]
pub struct ElevenLabs {
    client: reqwest::Client,
}

impl ElevenLabs {
    pub const ID: &'static str = "elevenlabs";
}

/// The speaking models in a models listing, in the order the provider
/// gave them. Anything that cannot speak is left out: the listing also
/// carries speech-to-speech models, which this setting cannot use.
fn speaking_models(body: &str) -> Result<Vec<SpeechModel>, TtsError> {
    let listed: Vec<serde_json::Value> = serde_json::from_str(body)
        .map_err(|e| TtsError::Provider(format!("the model list could not be read: {e}")))?;
    Ok(listed
        .into_iter()
        .filter(|model| {
            model
                .get("can_do_text_to_speech")
                .and_then(serde_json::Value::as_bool)
                .unwrap_or(false)
        })
        .filter_map(|model| {
            let id = model.get("model_id")?.as_str()?.to_string();
            let name = model
                .get("name")
                .and_then(serde_json::Value::as_str)
                .unwrap_or(&id)
                .to_string();
            Some(SpeechModel { id, name })
        })
        .collect())
}

/// What the frames are, from the format that was asked for. A provider
/// names its formats its own way, so this maps the ones whose framing we
/// can describe and passes anything else through under its own name.
fn codec_of(format: &str) -> (String, u32) {
    if let Some(rate) = format.strip_prefix("pcm_") {
        return (
            PCM16_CODEC.to_string(),
            rate.parse().unwrap_or(DEFAULT_SAMPLE_RATE),
        );
    }
    let (name, rate) = match format.split_once('_') {
        Some((name, rest)) => (
            name,
            rest.split('_')
                .next()
                .and_then(|r| r.parse().ok())
                .unwrap_or(0),
        ),
        None => (format, 0),
    };
    (name.to_string(), rate)
}

#[async_trait::async_trait]
impl TtsProvider for ElevenLabs {
    fn id(&self) -> &'static str {
        Self::ID
    }

    fn default_format(&self) -> &'static str {
        DEFAULT_FORMAT
    }

    async fn speak(&self, request: SpeakRequest<'_>) -> Result<SpeechStream, TtsError> {
        if request.voice.is_empty() {
            return Err(TtsError::NotConfigured(
                "no voice is chosen for speech".into(),
            ));
        }
        let format = if request.format.is_empty() {
            self.default_format()
        } else {
            request.format
        };
        let model = if request.model.is_empty() {
            DEFAULT_MODEL
        } else {
            request.model
        };
        let url = format!(
            "{}/v1/text-to-speech/{}/stream?output_format={format}",
            request.base_url.trim_end_matches('/'),
            request.voice
        );
        let response = self
            .client
            .post(&url)
            .header("xi-api-key", request.api_key)
            .json(&serde_json::json!({ "text": request.text, "model_id": model }))
            .send()
            .await
            .map_err(|e| TtsError::Unreachable(e.to_string()))?;
        let status = response.status();
        if !status.is_success() {
            // The body carries the provider's own reason, which is what
            // an operator needs to fix a key or a voice id.
            let detail = response.text().await.unwrap_or_default();
            let detail = detail.chars().take(400).collect::<String>();
            return Err(TtsError::Provider(format!(
                "speech was refused with {status}: {detail}"
            )));
        }
        let (codec, sample_rate) = codec_of(format);
        let (tx, frames) = frame_channel();
        let mut response = response;
        tokio::spawn(async move {
            loop {
                match response.chunk().await {
                    Ok(Some(bytes)) if bytes.is_empty() => continue,
                    Ok(Some(bytes)) => {
                        // A send that fails means the listener stopped,
                        // which is how an interruption stops paying for
                        // the rest: dropping the response closes the
                        // connection to the provider.
                        if tx.send(Ok(bytes)).await.is_err() {
                            debug!("speech was abandoned before it finished");
                            return;
                        }
                    }
                    Ok(None) => return,
                    Err(e) => {
                        warn!(error = %e, "the speech stream failed partway through");
                        let _ = tx.send(Err(TtsError::Provider(e.to_string()))).await;
                        return;
                    }
                }
            }
        });
        Ok(SpeechStream {
            codec,
            sample_rate,
            frames,
        })
    }

    async fn models(&self, account: ProviderAccount<'_>) -> Result<Vec<SpeechModel>, TtsError> {
        let url = format!("{}/v1/models", account.base_url.trim_end_matches('/'));
        let response = self
            .client
            .get(&url)
            .header("xi-api-key", account.api_key)
            .send()
            .await
            .map_err(|e| TtsError::Unreachable(e.to_string()))?;
        let status = response.status();
        let body = response.text().await.unwrap_or_default();
        if !status.is_success() {
            // The provider's own words, which name the missing
            // permission when a key is scoped to speaking only.
            let detail = body.chars().take(400).collect::<String>();
            return Err(TtsError::Provider(format!(
                "the model list was refused with {status}: {detail}"
            )));
        }
        speaking_models(&body)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The listing carries models that cannot speak, and the setting
    /// they would be chosen for can only speak, so they are left out.
    #[test]
    fn only_the_models_that_can_speak_are_offered() {
        let body = r#"[
            {"model_id":"eleven_v4_turbo","name":"Eleven v4 Turbo","can_do_text_to_speech":true},
            {"model_id":"eleven_english_sts_v2","name":"Speech to speech","can_do_text_to_speech":false},
            {"model_id":"eleven_flash_v2_5","name":"Eleven Flash v2.5","can_do_text_to_speech":true}
        ]"#;
        let models = speaking_models(body).expect("the listing parses");
        assert_eq!(
            models.iter().map(|m| m.id.as_str()).collect::<Vec<_>>(),
            vec!["eleven_v4_turbo", "eleven_flash_v2_5"]
        );
        assert_eq!(models[0].name, "Eleven v4 Turbo");
    }

    /// A model with no name is still selectable, named by its id, which
    /// is better than dropping it from the list.
    #[test]
    fn a_model_without_a_name_is_named_by_its_id() {
        let body = r#"[{"model_id":"eleven_new","can_do_text_to_speech":true}]"#;
        let models = speaking_models(body).expect("the listing parses");
        assert_eq!(models[0].id, "eleven_new");
        assert_eq!(models[0].name, "eleven_new");
    }

    #[test]
    fn a_listing_that_is_not_a_listing_says_so() {
        let error = speaking_models("not json").expect_err("this cannot be read");
        assert!(error.to_string().contains("could not be read"), "{error}");
    }

    #[test]
    fn a_pcm_format_names_its_framing_and_rate() {
        assert_eq!(codec_of("pcm_24000"), (PCM16_CODEC.to_string(), 24_000));
        assert_eq!(codec_of("pcm_16000"), (PCM16_CODEC.to_string(), 16_000));
    }

    #[test]
    fn a_compressed_format_keeps_its_own_name() {
        assert_eq!(codec_of("mp3_44100_128"), ("mp3".to_string(), 44_100));
        assert_eq!(codec_of("opus_48000_64"), ("opus".to_string(), 48_000));
    }

    /// A format this does not recognize still has to name something, so a
    /// client is told what it is being sent rather than nothing.
    #[test]
    fn an_unrecognized_format_passes_through_under_its_own_name() {
        assert_eq!(codec_of("ulaw"), ("ulaw".to_string(), 0));
    }
}
