//! The user's application appearance.
//!
//! Only an explicit choice is stored. No stored value means the browser
//! follows the operating system's `prefers-color-scheme`, so "system" is
//! the absence of a row rather than a third value to keep in step.

use serde::{Deserialize, Serialize};

pub const USER_APPEARANCE_KEY: &str = "app.appearance";

/// A stored value is one short JSON string, so anything larger is a
/// client sending something this setting was never meant to hold.
pub const APPEARANCE_MAX_BYTES: usize = 32;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Appearance {
    Light,
    Dark,
}

#[derive(Debug, thiserror::Error, PartialEq, Eq)]
pub enum AppearanceError {
    #[error("appearance must be at most {APPEARANCE_MAX_BYTES} bytes")]
    TooLarge,
    #[error("appearance must be \"light\" or \"dark\": {0}")]
    Malformed(String),
}

/// Parses an explicit appearance choice and returns one stable
/// representation suitable for storage and synchronization.
pub fn normalize_appearance(input: &[u8]) -> Result<(Appearance, String), AppearanceError> {
    if input.len() > APPEARANCE_MAX_BYTES {
        return Err(AppearanceError::TooLarge);
    }
    let appearance: Appearance = serde_json::from_slice(input)
        .map_err(|error| AppearanceError::Malformed(error.to_string()))?;
    let normalized = serde_json::to_string(&appearance)
        .map_err(|error| AppearanceError::Malformed(error.to_string()))?;
    Ok((appearance, normalized))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn accepts_the_two_explicit_choices_and_normalizes_them() {
        assert_eq!(
            normalize_appearance(b"\"light\"").unwrap(),
            (Appearance::Light, "\"light\"".to_string()),
        );
        assert_eq!(
            normalize_appearance(b"  \"dark\"  ").unwrap(),
            (Appearance::Dark, "\"dark\"".to_string()),
        );
    }

    #[test]
    fn rejects_anything_that_is_not_one_of_them() {
        for input in [
            &b"\"system\""[..],
            &b"\"Light\""[..],
            &b"light"[..],
            &b"null"[..],
            &b"{\"appearance\":\"light\"}"[..],
            &b"\"\""[..],
        ] {
            assert!(
                matches!(
                    normalize_appearance(input),
                    Err(AppearanceError::Malformed(_))
                ),
                "accepted {:?}",
                String::from_utf8_lossy(input),
            );
        }
    }

    #[test]
    fn rejects_a_value_larger_than_the_setting_can_hold() {
        let padded = format!("\"{}\"", "light".repeat(20));
        assert_eq!(
            normalize_appearance(padded.as_bytes()),
            Err(AppearanceError::TooLarge),
        );
    }
}
