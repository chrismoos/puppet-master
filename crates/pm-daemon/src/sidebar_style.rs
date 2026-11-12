//! How the web sidebar switches between buckets.
//!
//! Only an explicit choice is stored. No stored value means the browser
//! shows the rail.

use serde::{Deserialize, Serialize};

pub const USER_SIDEBAR_STYLE_KEY: &str = "app.sidebar_style";

/// A stored value is one short JSON string.
pub const SIDEBAR_STYLE_MAX_BYTES: usize = 32;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum SidebarStyle {
    Dropdown,
    Rail,
}

#[derive(Debug, thiserror::Error, PartialEq, Eq)]
pub enum SidebarStyleError {
    #[error("sidebar style must be at most {SIDEBAR_STYLE_MAX_BYTES} bytes")]
    TooLarge,
    #[error("sidebar style must be \"dropdown\" or \"rail\": {0}")]
    Malformed(String),
}

/// Parses an explicit sidebar style and returns one stable representation
/// suitable for storage and synchronization.
pub fn normalize_sidebar_style(input: &[u8]) -> Result<(SidebarStyle, String), SidebarStyleError> {
    if input.len() > SIDEBAR_STYLE_MAX_BYTES {
        return Err(SidebarStyleError::TooLarge);
    }
    let style: SidebarStyle = serde_json::from_slice(input)
        .map_err(|error| SidebarStyleError::Malformed(error.to_string()))?;
    let normalized = serde_json::to_string(&style)
        .map_err(|error| SidebarStyleError::Malformed(error.to_string()))?;
    Ok((style, normalized))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn accepts_each_style_and_normalizes_it() {
        assert_eq!(
            normalize_sidebar_style(b"\"dropdown\"").unwrap(),
            (SidebarStyle::Dropdown, "\"dropdown\"".to_string()),
        );
        assert_eq!(
            normalize_sidebar_style(b"  \"rail\"\n").unwrap(),
            (SidebarStyle::Rail, "\"rail\"".to_string()),
        );
    }

    #[test]
    fn rejects_anything_else() {
        for input in [
            &b"\"Rail\""[..],
            &b"\"tabs\""[..],
            &b"rail"[..],
            &b"null"[..],
            &b"{\"style\":\"rail\"}"[..],
        ] {
            assert!(
                matches!(
                    normalize_sidebar_style(input),
                    Err(SidebarStyleError::Malformed(_))
                ),
                "accepted {:?}",
                String::from_utf8_lossy(input),
            );
        }
    }

    #[test]
    fn rejects_an_oversized_value_before_parsing_it() {
        let padded = format!("\"rail\"{}", " ".repeat(SIDEBAR_STYLE_MAX_BYTES));
        assert_eq!(
            normalize_sidebar_style(padded.as_bytes()),
            Err(SidebarStyleError::TooLarge)
        );
    }
}
