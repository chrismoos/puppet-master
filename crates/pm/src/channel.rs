//! `pm update`, and the release channel a host follows.
//!
//! The channel is kept in a small file in the config directory rather
//! than in the daemon, because `pm update` has to work on a host whose
//! daemon is stopped, and because the daemon reads the same file to know
//! which pointer to poll.

use std::path::Path;

use pm_daemon::update::{self, Source, STABLE_CHANNEL};
use serde::{Deserialize, Serialize};

#[derive(Debug, Default, Serialize, Deserialize, PartialEq, Eq)]
struct UpdateConfig {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    channel: Option<String>,
}

/// The saved channel, or None when the host follows stable. A file that
/// names `stable` reads as None too.
pub fn saved(path: &Path) -> anyhow::Result<Option<String>> {
    let text = match std::fs::read_to_string(path) {
        Ok(text) => text,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(None),
        Err(e) => return Err(anyhow::anyhow!("reading {}: {e}", path.display())),
    };
    let config: UpdateConfig = toml::from_str(&text)
        .map_err(|e| anyhow::anyhow!("{} is not an update config: {e}", path.display()))?;
    Ok(config.channel.filter(|name| name != STABLE_CHANNEL))
}

/// Records the channel. `stable` or None removes the file, since stable
/// is what a host with no file follows.
pub fn save(path: &Path, channel: Option<&str>) -> anyhow::Result<()> {
    let channel = channel.filter(|name| *name != STABLE_CHANNEL);
    let Some(channel) = channel else {
        return match std::fs::remove_file(path) {
            Ok(()) => Ok(()),
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(()),
            Err(e) => Err(anyhow::anyhow!("removing {}: {e}", path.display())),
        };
    };
    if let Some(dir) = path.parent() {
        std::fs::create_dir_all(dir)?;
    }
    let config = UpdateConfig {
        channel: Some(channel.to_string()),
    };
    std::fs::write(path, toml::to_string(&config)?)
        .map_err(|e| anyhow::anyhow!("writing {}: {e}", path.display()))?;
    Ok(())
}

/// What `pm update` was asked to do, decided from the flags and the saved
/// channel before anything is fetched.
#[derive(Debug, PartialEq, Eq)]
pub enum Plan<'a> {
    /// Install this version whatever it is, and leave the saved channel
    /// alone.
    Exact(&'a str),
    /// Move to the newest build on the host's channel, forward only.
    Follow { channel: Option<&'a str> },
    /// Move to the newest build on another channel, backwards if that is
    /// where it points, and remember the channel.
    Switch { channel: Option<&'a str> },
}

/// A `--channel` value as a saved channel: `stable` is no channel.
fn normalize(channel: Option<&str>) -> Option<&str> {
    channel.filter(|name| *name != STABLE_CHANNEL)
}

pub fn plan<'a>(
    version: Option<&'a str>,
    requested: Option<&'a str>,
    saved: Option<&'a str>,
) -> anyhow::Result<Plan<'a>> {
    if let Some(name) = requested {
        if name != STABLE_CHANNEL && !update::is_valid_channel_name(name) {
            anyhow::bail!(
                "{name:?} is not a channel name: lowercase letters and digits, starting with a letter, or `stable`"
            );
        }
    }
    if let Some(version) = version {
        if requested.is_some() {
            anyhow::bail!("--version and --channel each choose what to install, pass only one");
        }
        return Ok(Plan::Exact(version));
    }
    let explicit = requested.is_some();
    let requested = normalize(requested);
    if !explicit || requested == saved {
        return Ok(Plan::Follow { channel: saved });
    }
    Ok(Plan::Switch { channel: requested })
}

fn channel_label(channel: Option<&str>) -> &str {
    channel.unwrap_or(STABLE_CHANNEL)
}

pub async fn run_update(
    check: bool,
    version: Option<&str>,
    requested: Option<&str>,
) -> anyhow::Result<()> {
    let current = update::release_version(pm_daemon::pm_build_version());
    if !update::updates_enabled() {
        anyhow::bail!(
            "this build cannot verify releases, so it will not update itself (running {current})"
        );
    }
    let config_path = crate::paths::update_config_path();
    let saved = saved(&config_path)?;
    let plan = plan(version, requested, saved.as_deref())?;
    let client = reqwest::Client::builder()
        .user_agent(format!("pm/{}", pm_daemon::pm_build_version()))
        .build()?;
    let release = match &plan {
        Plan::Exact(version) => update::fetch_release(&client, Source::Exact(version)).await?,
        Plan::Follow { channel } | Plan::Switch { channel } => {
            update::fetch_release(&client, Source::for_channel(*channel)).await?
        }
    };
    match &plan {
        Plan::Follow { channel } => {
            if !update::is_upgrade(current, &release.version) {
                println!(
                    "pm {current} is current on {}; newest published there is {}",
                    channel_label(*channel),
                    release.version
                );
                return Ok(());
            }
        }
        Plan::Switch { channel } => {
            if release.version == current {
                save(&config_path, *channel)?;
                println!(
                    "pm {current} is what {} points at; now following {}",
                    channel_label(*channel),
                    channel_label(*channel)
                );
                return Ok(());
            }
            if !update::is_upgrade(current, &release.version) {
                println!(
                    "moving back from {current} to {}, which is what {} points at",
                    release.version,
                    channel_label(*channel)
                );
            }
        }
        Plan::Exact(_) => {}
    }
    if check {
        println!(
            "pm {} is available on {} (running {current})",
            release.version,
            channel_label(match &plan {
                Plan::Exact(_) => None,
                Plan::Follow { channel } | Plan::Switch { channel } => *channel,
            })
        );
        return Ok(());
    }
    let binary = update::download_verified(&client, &release).await?;
    let path = update::install_over_current_exe(&binary)?;
    if let Plan::Switch { channel } = &plan {
        save(&config_path, *channel)?;
    }
    println!(
        "installed pm {} at {}. A running daemon keeps its current build until you restart it.",
        release.version,
        path.display()
    );
    if let Plan::Switch { channel } = &plan {
        println!("this host now follows {}", channel_label(*channel));
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_saved_channel_round_trips_and_stable_means_no_file() {
        let tmp = tempfile::tempdir().unwrap();
        let path = tmp.path().join("cfg").join("update.toml");
        assert_eq!(saved(&path).unwrap(), None);
        save(&path, Some("dev")).unwrap();
        assert_eq!(saved(&path).unwrap(), Some("dev".to_string()));
        assert_eq!(
            std::fs::read_to_string(&path).unwrap(),
            "channel = \"dev\"\n"
        );
        save(&path, Some("stable")).unwrap();
        assert!(!path.exists());
        save(&path, Some("dev")).unwrap();
        save(&path, None).unwrap();
        assert!(!path.exists());
        std::fs::write(&path, "channel = \"stable\"\n").unwrap();
        assert_eq!(saved(&path).unwrap(), None);
    }

    #[test]
    fn a_bare_update_follows_the_saved_channel() {
        assert_eq!(
            plan(None, None, None).unwrap(),
            Plan::Follow { channel: None }
        );
        assert_eq!(
            plan(None, None, Some("dev")).unwrap(),
            Plan::Follow {
                channel: Some("dev")
            }
        );
        assert_eq!(
            plan(None, Some("dev"), Some("dev")).unwrap(),
            Plan::Follow {
                channel: Some("dev")
            }
        );
        assert_eq!(
            plan(None, Some("stable"), None).unwrap(),
            Plan::Follow { channel: None }
        );
    }

    #[test]
    fn naming_another_channel_switches_to_it() {
        assert_eq!(
            plan(None, Some("dev"), None).unwrap(),
            Plan::Switch {
                channel: Some("dev")
            }
        );
        assert_eq!(
            plan(None, Some("stable"), Some("dev")).unwrap(),
            Plan::Switch { channel: None }
        );
        assert_eq!(
            plan(None, Some("foo"), Some("dev")).unwrap(),
            Plan::Switch {
                channel: Some("foo")
            }
        );
    }

    #[test]
    fn an_exact_version_ignores_channels_and_refuses_both_flags() {
        assert_eq!(
            plan(Some("0.10.0-dev.3"), None, Some("foo")).unwrap(),
            Plan::Exact("0.10.0-dev.3")
        );
        assert!(plan(Some("0.10.0"), Some("dev"), None).is_err());
        assert!(plan(None, Some("Dev"), None).is_err());
        assert!(plan(None, Some("latest"), None).is_err());
    }
}
