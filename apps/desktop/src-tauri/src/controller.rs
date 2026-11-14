//! The controller this app opens, remembered between launches.

use std::path::PathBuf;

use serde::{Deserialize, Serialize};
use tauri::{AppHandle, Manager, Url};

const SETTINGS_FILE: &str = "controller.json";

#[derive(Serialize, Deserialize)]
struct Settings {
    url: String,
}

fn settings_path(app: &AppHandle) -> Option<PathBuf> {
    app.path()
        .app_config_dir()
        .ok()
        .map(|dir| dir.join(SETTINGS_FILE))
}

/// Accepts what a person types: a bare host gets `https://`, and anything
/// that is not an http or https URL is refused.
pub fn parse(input: &str) -> Result<Url, String> {
    let trimmed = input.trim();
    if trimmed.is_empty() {
        return Err("Enter your controller's address.".into());
    }
    let with_scheme = if trimmed.contains("://") {
        trimmed.to_string()
    } else {
        format!("https://{trimmed}")
    };
    let url =
        Url::parse(&with_scheme).map_err(|error| format!("That is not an address: {error}"))?;
    if !matches!(url.scheme(), "http" | "https") || url.host_str().is_none() {
        return Err("Use an http or https address.".into());
    }
    Ok(url)
}

pub fn load(app: &AppHandle) -> Option<Url> {
    let text = std::fs::read_to_string(settings_path(app)?).ok()?;
    let settings: Settings = serde_json::from_str(&text).ok()?;
    parse(&settings.url).ok()
}

pub fn save(app: &AppHandle, url: &Url) -> std::io::Result<()> {
    let path =
        settings_path(app).ok_or_else(|| std::io::Error::other("no configuration directory"))?;
    if let Some(dir) = path.parent() {
        std::fs::create_dir_all(dir)?;
    }
    let text = serde_json::to_string_pretty(&Settings {
        url: url.to_string(),
    })?;
    std::fs::write(path, text)
}

#[cfg(test)]
mod tests {
    use super::parse;

    #[test]
    fn a_bare_host_is_taken_as_https() {
        assert_eq!(
            parse("pm.example.test").unwrap().as_str(),
            "https://pm.example.test/"
        );
        assert_eq!(
            parse(" http://127.0.0.1:7600 ").unwrap().as_str(),
            "http://127.0.0.1:7600/"
        );
    }

    #[test]
    fn only_http_addresses_are_accepted() {
        assert!(parse("").is_err());
        assert!(parse("file:///etc/passwd").is_err());
        assert!(parse("javascript:alert(1)").is_err());
    }
}
