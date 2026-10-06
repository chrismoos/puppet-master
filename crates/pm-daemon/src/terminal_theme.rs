//! Canonical Puppet Master terminal-theme schema and server-side validation.
//!
//! Imports are parsed in the browser, but only this normalized native format is
//! ever persisted. Keeping the daemon authoritative prevents a modified client
//! from storing colors or fields xterm was never meant to receive.

use serde::{Deserialize, Serialize};

pub const USER_TERMINAL_THEME_KEY: &str = "terminal.theme";
pub const TERMINAL_THEME_KIND: &str = "puppet-master-terminal-theme";
pub const TERMINAL_THEME_VERSION: u32 = 1;
pub const TERMINAL_THEME_MAX_BYTES: usize = 64 * 1024;

const NAME_MAX_CHARS: usize = 80;
const METADATA_MAX_CHARS: usize = 160;
const EXTENDED_ANSI_COLORS: usize = 240;

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct TerminalTheme {
    pub kind: String,
    pub version: u32,
    pub name: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub author: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub license: Option<String>,
    pub colors: TerminalThemeColors,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct TerminalThemeColors {
    pub foreground: String,
    pub background: String,
    pub cursor: String,
    pub cursor_accent: String,
    pub selection_foreground: String,
    pub selection_background: String,
    pub black: String,
    pub red: String,
    pub green: String,
    pub yellow: String,
    pub blue: String,
    pub magenta: String,
    pub cyan: String,
    pub white: String,
    pub bright_black: String,
    pub bright_red: String,
    pub bright_green: String,
    pub bright_yellow: String,
    pub bright_blue: String,
    pub bright_magenta: String,
    pub bright_cyan: String,
    pub bright_white: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub selection_inactive_background: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub scrollbar_slider_background: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub scrollbar_slider_hover_background: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub scrollbar_slider_active_background: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub overview_ruler_border: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub extended_ansi: Option<Vec<String>>,
}

#[derive(Debug, thiserror::Error, PartialEq, Eq)]
pub enum TerminalThemeError {
    #[error("theme exceeds the 64 KiB import limit")]
    TooLarge,
    #[error("theme is not valid UTF-8 JSON: {0}")]
    Malformed(String),
    #[error("unsupported theme kind {0:?}")]
    Kind(String),
    #[error("unsupported terminal theme version {0}")]
    Version(u32),
    #[error("theme name must contain 1–{NAME_MAX_CHARS} characters")]
    Name,
    #[error("theme {0} must contain at most {METADATA_MAX_CHARS} characters")]
    Metadata(&'static str),
    #[error("theme color {field} must be {expected}, not {value:?}")]
    Color {
        field: String,
        expected: &'static str,
        value: String,
    },
    #[error("extendedAnsi must contain all 240 colors (ANSI 16–255), not {0}")]
    ExtendedAnsiLength(usize),
}

/// Parses strict native JSON, validates every bound/color, and returns one
/// stable compact representation suitable for storage and synchronization.
pub fn normalize_terminal_theme(
    input: &[u8],
) -> Result<(TerminalTheme, String), TerminalThemeError> {
    if input.len() > TERMINAL_THEME_MAX_BYTES {
        return Err(TerminalThemeError::TooLarge);
    }
    let mut theme: TerminalTheme = serde_json::from_slice(input)
        .map_err(|error| TerminalThemeError::Malformed(error.to_string()))?;
    validate_and_normalize(&mut theme)?;
    let normalized = serde_json::to_string(&theme)
        .map_err(|error| TerminalThemeError::Malformed(error.to_string()))?;
    if normalized.len() > TERMINAL_THEME_MAX_BYTES {
        return Err(TerminalThemeError::TooLarge);
    }
    Ok((theme, normalized))
}

fn validate_and_normalize(theme: &mut TerminalTheme) -> Result<(), TerminalThemeError> {
    if theme.kind != TERMINAL_THEME_KIND {
        return Err(TerminalThemeError::Kind(theme.kind.clone()));
    }
    if theme.version != TERMINAL_THEME_VERSION {
        return Err(TerminalThemeError::Version(theme.version));
    }
    let name_chars = theme.name.chars().count();
    if theme.name.trim().is_empty() || name_chars > NAME_MAX_CHARS {
        return Err(TerminalThemeError::Name);
    }
    validate_metadata("author", theme.author.as_deref())?;
    validate_metadata("license", theme.license.as_deref())?;

    let colors = &mut theme.colors;
    for (field, value) in [
        ("foreground", &mut colors.foreground),
        ("background", &mut colors.background),
        ("cursor", &mut colors.cursor),
        ("cursorAccent", &mut colors.cursor_accent),
        ("selectionForeground", &mut colors.selection_foreground),
        ("black", &mut colors.black),
        ("red", &mut colors.red),
        ("green", &mut colors.green),
        ("yellow", &mut colors.yellow),
        ("blue", &mut colors.blue),
        ("magenta", &mut colors.magenta),
        ("cyan", &mut colors.cyan),
        ("white", &mut colors.white),
        ("brightBlack", &mut colors.bright_black),
        ("brightRed", &mut colors.bright_red),
        ("brightGreen", &mut colors.bright_green),
        ("brightYellow", &mut colors.bright_yellow),
        ("brightBlue", &mut colors.bright_blue),
        ("brightMagenta", &mut colors.bright_magenta),
        ("brightCyan", &mut colors.bright_cyan),
        ("brightWhite", &mut colors.bright_white),
    ] {
        normalize_color(field, value, false)?;
    }
    normalize_color(
        "selectionBackground",
        &mut colors.selection_background,
        true,
    )?;
    normalize_optional_color(
        "selectionInactiveBackground",
        &mut colors.selection_inactive_background,
        true,
    )?;
    normalize_optional_color(
        "scrollbarSliderBackground",
        &mut colors.scrollbar_slider_background,
        false,
    )?;
    normalize_optional_color(
        "scrollbarSliderHoverBackground",
        &mut colors.scrollbar_slider_hover_background,
        false,
    )?;
    normalize_optional_color(
        "scrollbarSliderActiveBackground",
        &mut colors.scrollbar_slider_active_background,
        false,
    )?;
    normalize_optional_color(
        "overviewRulerBorder",
        &mut colors.overview_ruler_border,
        false,
    )?;
    if let Some(extended) = &mut colors.extended_ansi {
        if extended.len() != EXTENDED_ANSI_COLORS {
            return Err(TerminalThemeError::ExtendedAnsiLength(extended.len()));
        }
        for (offset, value) in extended.iter_mut().enumerate() {
            normalize_color(&format!("extendedAnsi[{}]", offset + 16), value, false)?;
        }
    }
    Ok(())
}

fn validate_metadata(field: &'static str, value: Option<&str>) -> Result<(), TerminalThemeError> {
    if value.is_some_and(|value| value.chars().count() > METADATA_MAX_CHARS) {
        return Err(TerminalThemeError::Metadata(field));
    }
    Ok(())
}

fn normalize_optional_color(
    field: &str,
    value: &mut Option<String>,
    alpha: bool,
) -> Result<(), TerminalThemeError> {
    if let Some(value) = value {
        normalize_color(field, value, alpha)?;
    }
    Ok(())
}

fn normalize_color(field: &str, value: &mut String, alpha: bool) -> Result<(), TerminalThemeError> {
    let valid_len = value.len() == 7 || (alpha && value.len() == 9);
    let valid = valid_len
        && value.starts_with('#')
        && value.as_bytes()[1..].iter().all(u8::is_ascii_hexdigit);
    if !valid {
        return Err(TerminalThemeError::Color {
            field: field.to_string(),
            expected: if alpha {
                "#RRGGBB or #RRGGBBAA"
            } else {
                "#RRGGBB"
            },
            value: value.clone(),
        });
    }
    value.make_ascii_lowercase();
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn native_json() -> serde_json::Value {
        serde_json::json!({
            "kind": TERMINAL_THEME_KIND,
            "version": 1,
            "name": "Example",
            "author": "PM",
            "colors": {
                "foreground": "#C9CEDA", "background": "#0B0E14",
                "cursor": "#FFB224", "cursorAccent": "#0B0E14",
                "selectionForeground": "#EEF1F6", "selectionBackground": "#2B3548CC",
                "black": "#2E3436", "red": "#CC0000", "green": "#4E9A06",
                "yellow": "#C4A000", "blue": "#3465A4", "magenta": "#75507B",
                "cyan": "#06989A", "white": "#D3D7CF", "brightBlack": "#555753",
                "brightRed": "#EF2929", "brightGreen": "#8AE234",
                "brightYellow": "#FCE94F", "brightBlue": "#729FCF",
                "brightMagenta": "#AD7FA8", "brightCyan": "#34E2E2",
                "brightWhite": "#EEEEEC"
            }
        })
    }

    #[test]
    fn native_theme_round_trips_to_normalized_json() {
        let input = serde_json::to_vec_pretty(&native_json()).unwrap();
        let (theme, normalized) = normalize_terminal_theme(&input).unwrap();
        assert_eq!(theme.colors.selection_background, "#2b3548cc");
        let (again, second) = normalize_terminal_theme(normalized.as_bytes()).unwrap();
        assert_eq!(again, theme);
        assert_eq!(second, normalized);
    }

    #[test]
    fn shared_typescript_fixture_matches_the_rust_schema() {
        let fixture = include_bytes!(
            "../../../packages/client-core/src/theme/terminal-theme-v1.fixture.json"
        );
        let (theme, normalized) = normalize_terminal_theme(fixture).unwrap();
        assert_eq!(theme.name, "Schema parity fixture");
        assert_eq!(theme.colors.selection_background, "#2b3548cc");
        assert_eq!(
            theme.colors.overview_ruler_border.as_deref(),
            Some("#667788")
        );
        assert_eq!(
            normalize_terminal_theme(normalized.as_bytes()).unwrap().1,
            normalized
        );
    }

    #[test]
    fn rejects_unknown_duplicate_and_malformed_fields() {
        let mut unknown = native_json();
        unknown["colors"]["link"] = serde_json::json!("#ffffff");
        assert!(matches!(
            normalize_terminal_theme(&serde_json::to_vec(&unknown).unwrap()),
            Err(TerminalThemeError::Malformed(_))
        ));
        let duplicate = serde_json::to_string(&native_json()).unwrap().replacen(
            "\"version\":1",
            "\"version\":1,\"version\":1",
            1,
        );
        assert!(matches!(
            normalize_terminal_theme(duplicate.as_bytes()),
            Err(TerminalThemeError::Malformed(_))
        ));
        assert!(matches!(
            normalize_terminal_theme(&[0xff]),
            Err(TerminalThemeError::Malformed(_))
        ));
    }

    #[test]
    fn extended_ansi_is_all_or_nothing() {
        let mut partial = native_json();
        partial["colors"]["extendedAnsi"] = serde_json::json!(["#000000"]);
        assert_eq!(
            normalize_terminal_theme(&serde_json::to_vec(&partial).unwrap()).unwrap_err(),
            TerminalThemeError::ExtendedAnsiLength(1)
        );
        partial["colors"]["extendedAnsi"] =
            serde_json::Value::Array((0..240).map(|_| serde_json::json!("#123456")).collect());
        assert!(normalize_terminal_theme(&serde_json::to_vec(&partial).unwrap()).is_ok());
    }

    #[test]
    fn caps_metadata_input_and_color_shapes() {
        let mut input = native_json();
        input["name"] = serde_json::json!("x".repeat(NAME_MAX_CHARS + 1));
        assert_eq!(
            normalize_terminal_theme(&serde_json::to_vec(&input).unwrap()).unwrap_err(),
            TerminalThemeError::Name
        );
        let mut input = native_json();
        input["colors"]["background"] = serde_json::json!("#000000ff");
        assert!(matches!(
            normalize_terminal_theme(&serde_json::to_vec(&input).unwrap()),
            Err(TerminalThemeError::Color { .. })
        ));
        assert_eq!(
            normalize_terminal_theme(&vec![b' '; TERMINAL_THEME_MAX_BYTES + 1]).unwrap_err(),
            TerminalThemeError::TooLarge
        );
    }
}
