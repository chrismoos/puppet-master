//! The user's application UI theme.
//!
//! Only an explicit choice is stored. No stored value means the browser
//! follows the default "standard" theme, which it presents as Midnight.

use serde::{Deserialize, Serialize};

pub const USER_UI_THEME_KEY: &str = "app.theme";

/// A stored value is one short JSON string.
pub const UI_THEME_MAX_BYTES: usize = 32;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum UiTheme {
    Standard,
    Graphite,
    Studio,
    /// No longer offered. Clients that know Graphite read it as Graphite,
    /// and older clients still send and expect it unchanged.
    Compact,
}

#[derive(Debug, thiserror::Error, PartialEq, Eq)]
pub enum UiThemeError {
    #[error("ui theme must be at most {UI_THEME_MAX_BYTES} bytes")]
    TooLarge,
    #[error("ui theme must be \"standard\", \"graphite\", or \"studio\": {0}")]
    Malformed(String),
}

/// Parses an explicit theme choice and returns one stable
/// representation suitable for storage and synchronization.
pub fn normalize_ui_theme(input: &[u8]) -> Result<(UiTheme, String), UiThemeError> {
    if input.len() > UI_THEME_MAX_BYTES {
        return Err(UiThemeError::TooLarge);
    }
    let theme: UiTheme = serde_json::from_slice(input)
        .map_err(|error| UiThemeError::Malformed(error.to_string()))?;
    let normalized = serde_json::to_string(&theme)
        .map_err(|error| UiThemeError::Malformed(error.to_string()))?;
    Ok((theme, normalized))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn accepts_each_offered_choice_and_normalizes_it() {
        assert_eq!(
            normalize_ui_theme(b"\"standard\"").unwrap(),
            (UiTheme::Standard, "\"standard\"".to_string()),
        );
        assert_eq!(
            normalize_ui_theme(b"  \"graphite\"  ").unwrap(),
            (UiTheme::Graphite, "\"graphite\"".to_string()),
        );
        assert_eq!(
            normalize_ui_theme(b"\"studio\"\n").unwrap(),
            (UiTheme::Studio, "\"studio\"".to_string()),
        );
    }

    #[test]
    fn keeps_accepting_the_retired_compact_choice_unchanged() {
        assert_eq!(
            normalize_ui_theme(b"\"compact\"").unwrap(),
            (UiTheme::Compact, "\"compact\"".to_string()),
        );
    }

    #[test]
    fn rejects_anything_that_is_not_one_of_them() {
        for input in [
            &b"\"dark\""[..],
            &b"\"midnight\""[..],
            &b"\"Standard\""[..],
            &b"\"Graphite\""[..],
            &b"null"[..],
            &b"{\"theme\":\"standard\"}"[..],
            &b"\"\""[..],
        ] {
            assert!(
                matches!(normalize_ui_theme(input), Err(UiThemeError::Malformed(_))),
                "accepted {:?}",
                String::from_utf8_lossy(input),
            );
        }
    }

    #[test]
    fn rejects_a_value_larger_than_the_setting_can_hold() {
        let padded = format!("\"{}\"", "compact".repeat(20));
        assert_eq!(
            normalize_ui_theme(padded.as_bytes()),
            Err(UiThemeError::TooLarge),
        );
    }
}
