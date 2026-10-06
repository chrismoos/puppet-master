use std::path::PathBuf;

const APP_NAME: &str = "puppet-master";
const SOCKET_FILE: &str = "pm.sock";
const DB_FILE: &str = "pm.db";

pub fn data_dir() -> PathBuf {
    directories::ProjectDirs::from("", "", APP_NAME)
        .map(|d| d.data_dir().to_path_buf())
        .unwrap_or_else(|| PathBuf::from(".").join(format!(".{APP_NAME}")))
}

pub fn default_socket_path() -> PathBuf {
    if let Ok(dir) = std::env::var("XDG_RUNTIME_DIR") {
        return PathBuf::from(dir).join(APP_NAME).join(SOCKET_FILE);
    }
    data_dir().join(SOCKET_FILE)
}

pub fn default_db_path() -> PathBuf {
    data_dir().join(DB_FILE)
}

pub fn default_scrollback_dir() -> PathBuf {
    data_dir().join("scrollback")
}

/// Where `pm worker` persists its controller URL and credential.
pub fn worker_config_path() -> PathBuf {
    directories::ProjectDirs::from("", "", APP_NAME)
        .map(|d| d.config_dir().join("worker.toml"))
        .unwrap_or_else(|| data_dir().join("worker.toml"))
}

/// Where `pm update --channel` remembers the release channel this host
/// follows. Read by `pm update` and by the daemon on this host.
pub fn update_config_path() -> PathBuf {
    directories::ProjectDirs::from("", "", APP_NAME)
        .map(|d| d.config_dir().join("update.toml"))
        .unwrap_or_else(|| data_dir().join("update.toml"))
}

/// Where `pm login` saves its logins to remote controllers, one file per
/// name.
pub fn remotes_dir() -> PathBuf {
    directories::ProjectDirs::from("", "", APP_NAME)
        .map(|d| d.config_dir().join("remotes"))
        .unwrap_or_else(|| data_dir().join("remotes"))
}

/// Where `pm worker` keeps the long-lived key that identifies this host to
/// its controllers. A controller tells hosts apart by this key, so each
/// named worker needs its own; the unnamed one keeps the original path so
/// a host enrolled before names stays the same host.
pub fn worker_key_path(name: Option<&str>) -> PathBuf {
    let file = match name {
        Some(name) => format!("worker-key-{}.pem", slug(name)),
        None => "worker-key.pem".to_string(),
    };
    directories::ProjectDirs::from("", "", APP_NAME)
        .map(|d| d.config_dir().join(&file))
        .unwrap_or_else(|| data_dir().join(&file))
}

/// A worker name reduced to what is safe in a file name.
fn slug(name: &str) -> String {
    name.chars()
        .map(|c| {
            if c.is_ascii_alphanumeric() || c == '-' || c == '_' {
                c
            } else {
                '-'
            }
        })
        .collect()
}

pub fn worker_transcript_dir(name: Option<&str>) -> PathBuf {
    match name {
        Some(name) => data_dir().join(format!("worker-transcripts-{}", slug(name))),
        None => data_dir().join("worker-transcripts"),
    }
}
