//! Fetching and applying signed pm releases.
//!
//! Every artifact is signed with the release key, whose public half is
//! compiled in below. A build whose key file was never filled in refuses
//! to update rather than falling back to trusting the transport, since
//! the thing being downloaded runs agents as the user.
//!
//! Signatures are raw Ed25519 over the artifact bytes, base64 encoded.
//! Verification goes through ring, which is already linked into every
//! build by the TLS stack.

use anyhow::{bail, Context, Result};
use base64::Engine as _;
use serde::Deserialize;
use sha2::{Digest, Sha256};
use std::path::{Path, PathBuf};

use crate::pm_build_version;

pub const RELEASE_BASE_URL: &str = "https://dl.puppet-master.xyz";

/// The release key's public half, base64 encoded.
const RELEASE_PUBLIC_KEY: &str = include_str!("release_pubkey.txt");
/// What the key file ships as before a release key exists.
const KEY_UNSET: &str = "unset";

const MANIFEST_NAME: &str = "release.json";
const LATEST_NAME: &str = "latest.json";
/// Where each channel's pointer lives: `channels/<name>.json`.
const CHANNELS_DIR: &str = "channels";

/// The channel name that means the stable pointer, `latest.json`. Never a
/// directory under `channels/`.
pub const STABLE_CHANNEL: &str = "stable";

/// Names a channel pointer can never have, because each already means
/// something in the store.
const RESERVED_CHANNEL_NAMES: &[&str] = &[STABLE_CHANNEL, "latest", CHANNELS_DIR];

/// One release, as published in `latest.json`, in a channel pointer, and
/// in each version's own `release.json`.
#[derive(Debug, Clone, Deserialize, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct Release {
    pub version: String,
    /// The worker protocol this build speaks. A worker compares it before
    /// spending bandwidth on a build it could not talk to anyway.
    pub protocol_version: u32,
    pub artifacts: std::collections::BTreeMap<String, Artifact>,
    /// The channel this build was published to. Absent for a stable
    /// release.
    #[serde(default)]
    pub channel: Option<String>,
    /// The pre-release a stable was cut from, when it was promoted rather
    /// than released directly.
    #[serde(default)]
    pub promoted_from: Option<String>,
}

/// Which manifest to read.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Source<'a> {
    /// The newest stable release, `latest.json`.
    Stable,
    /// The newest build on a channel, `channels/<name>.json`.
    Channel(&'a str),
    /// One version's own manifest.
    Exact(&'a str),
}

impl<'a> Source<'a> {
    /// The source a saved channel name selects: `stable` or no name is the
    /// stable pointer.
    pub fn for_channel(channel: Option<&'a str>) -> Self {
        match channel {
            None | Some(STABLE_CHANNEL) => Source::Stable,
            Some(name) => Source::Channel(name),
        }
    }

    fn url(self) -> String {
        match self {
            Source::Stable => format!("{RELEASE_BASE_URL}/{LATEST_NAME}"),
            Source::Channel(name) => format!("{RELEASE_BASE_URL}/{CHANNELS_DIR}/{name}.json"),
            Source::Exact(version) => format!("{RELEASE_BASE_URL}/v{version}/{MANIFEST_NAME}"),
        }
    }
}

/// Whether a channel name can be published and asked for: lowercase
/// letters and digits, starting with a letter, and not a name the store
/// already uses. `stable` is refused here because it is not a channel
/// pointer; `Source::for_channel` maps it to the stable one.
pub fn is_valid_channel_name(name: &str) -> bool {
    let shape = name.starts_with(|c: char| c.is_ascii_lowercase())
        && name
            .chars()
            .all(|c| c.is_ascii_lowercase() || c.is_ascii_digit());
    shape && !RESERVED_CHANNEL_NAMES.contains(&name)
}

#[derive(Debug, Clone, Deserialize, PartialEq)]
pub struct Artifact {
    pub path: String,
    pub sha256: String,
}

impl Release {
    pub fn artifact_for_this_host(&self) -> Result<&Artifact> {
        self.artifacts.get(target_triple()).with_context(|| {
            format!(
                "release {} publishes no build for {}",
                self.version,
                target_triple()
            )
        })
    }
}

/// The target this binary was built for, which names its artifact.
pub const fn target_triple() -> &'static str {
    #[cfg(all(target_os = "linux", target_arch = "x86_64"))]
    {
        "x86_64-unknown-linux-gnu"
    }
    #[cfg(all(target_os = "linux", target_arch = "aarch64"))]
    {
        "aarch64-unknown-linux-gnu"
    }
    #[cfg(all(target_os = "macos", target_arch = "aarch64"))]
    {
        "aarch64-apple-darwin"
    }
    #[cfg(not(any(
        all(target_os = "linux", target_arch = "x86_64"),
        all(target_os = "linux", target_arch = "aarch64"),
        all(target_os = "macos", target_arch = "aarch64")
    )))]
    {
        "unsupported"
    }
}

/// Whether this build can verify a release at all.
pub fn updates_enabled() -> bool {
    RELEASE_PUBLIC_KEY.trim() != KEY_UNSET && target_triple() != "unsupported"
}

/// The release part of a build identity: `0.2.0` out of `0.2.0+abc1234`.
pub fn release_version(build_version: &str) -> &str {
    build_version.split('+').next().unwrap_or(build_version)
}

/// One dot-separated identifier of a pre-release suffix. Numeric ones
/// order numerically and before alphanumeric ones, as semver says.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord)]
enum PreRelease {
    Numeric(u64),
    Alphanumeric(String),
}

/// A release version: `major.minor.patch`, with an optional pre-release
/// suffix such as `-dev.3`. Build metadata is stripped before parsing.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Version {
    major: u64,
    minor: u64,
    patch: u64,
    pre: Vec<PreRelease>,
}

impl Version {
    pub fn parse(text: &str) -> Option<Self> {
        let release = release_version(text);
        let (core, pre) = match release.split_once('-') {
            Some((core, pre)) => (core, Some(pre)),
            None => (release, None),
        };
        let mut it = core.split('.');
        let major = plain_number(it.next()?)?;
        let minor = plain_number(it.next()?)?;
        let patch = plain_number(it.next()?)?;
        if it.next().is_some() {
            return None;
        }
        let pre = match pre {
            None => Vec::new(),
            Some(pre) => pre
                .split('.')
                .map(|id| {
                    if id.is_empty() || !id.chars().all(|c| c.is_ascii_alphanumeric() || c == '-') {
                        return None;
                    }
                    Some(match plain_number(id) {
                        Some(n) => PreRelease::Numeric(n),
                        None if id.chars().all(|c| c.is_ascii_digit()) => return None,
                        None => PreRelease::Alphanumeric(id.to_string()),
                    })
                })
                .collect::<Option<Vec<_>>>()?,
        };
        Some(Self {
            major,
            minor,
            patch,
            pre,
        })
    }

    pub fn is_prerelease(&self) -> bool {
        !self.pre.is_empty()
    }

    /// The channel a pre-release names: its first alphanumeric identifier,
    /// `dev` in `0.10.0-dev.3`. A stable version names none.
    pub fn channel(&self) -> Option<&str> {
        self.pre.iter().find_map(|id| match id {
            PreRelease::Alphanumeric(name) => Some(name.as_str()),
            PreRelease::Numeric(_) => None,
        })
    }
}

/// A decimal number with no leading zero, which is what semver allows for
/// a version component and a numeric pre-release identifier.
fn plain_number(text: &str) -> Option<u64> {
    if text.is_empty() || !text.chars().all(|c| c.is_ascii_digit()) {
        return None;
    }
    if text.len() > 1 && text.starts_with('0') {
        return None;
    }
    text.parse().ok()
}

impl Ord for Version {
    fn cmp(&self, other: &Self) -> std::cmp::Ordering {
        let core =
            (self.major, self.minor, self.patch).cmp(&(other.major, other.minor, other.patch));
        if core != std::cmp::Ordering::Equal {
            return core;
        }
        // A pre-release precedes the release it leads to, and otherwise
        // identifiers compare left to right with the shorter list first.
        match (self.pre.is_empty(), other.pre.is_empty()) {
            (true, true) => std::cmp::Ordering::Equal,
            (true, false) => std::cmp::Ordering::Greater,
            (false, true) => std::cmp::Ordering::Less,
            (false, false) => self.pre.cmp(&other.pre),
        }
    }
}

impl PartialOrd for Version {
    fn partial_cmp(&self, other: &Self) -> Option<std::cmp::Ordering> {
        Some(self.cmp(other))
    }
}

/// Whether moving from `current` to `candidate` goes forward. An
/// unparsable version compares as no upgrade, so a malformed manifest
/// cannot walk a host backwards.
pub fn is_upgrade(current: &str, candidate: &str) -> bool {
    match (Version::parse(current), Version::parse(candidate)) {
        (Some(current), Some(candidate)) => candidate > current,
        _ => false,
    }
}

fn verify_with(public_key_base64: &str, payload: &[u8], signature: &str) -> Result<()> {
    let engine = base64::engine::general_purpose::STANDARD;
    let key = engine
        .decode(public_key_base64.trim())
        .context("the compiled-in release key is not valid base64")?;
    let signature = engine
        .decode(signature.trim())
        .context("release signature is not valid base64")?;
    ring::signature::UnparsedPublicKey::new(&ring::signature::ED25519, key)
        .verify(payload, &signature)
        .map_err(|_| anyhow::anyhow!("release signature does not match the release key"))
}

/// Refuses outright when the build carries no release key, rather than
/// falling back to trusting whatever the transport delivered.
fn verify_against(public_key: &str, payload: &[u8], signature: &str) -> Result<()> {
    if public_key.trim() == KEY_UNSET {
        bail!("this build carries no release key, so updates are disabled");
    }
    verify_with(public_key, payload, signature)
}

fn verify_signature(payload: &[u8], signature: &str) -> Result<()> {
    verify_against(RELEASE_PUBLIC_KEY, payload, signature)
}

fn verify_digest(payload: &[u8], expected: &str) -> Result<()> {
    let actual = hex::encode(Sha256::digest(payload));
    if !actual.eq_ignore_ascii_case(expected.trim()) {
        bail!("release digest is {actual}, expected {expected}");
    }
    Ok(())
}

async fn get(client: &reqwest::Client, url: &str) -> Result<Vec<u8>> {
    let response = client
        .get(url)
        .send()
        .await
        .with_context(|| format!("requesting {url}"))?;
    let status = response.status();
    if !status.is_success() {
        bail!("{url} returned {status}");
    }
    Ok(response
        .bytes()
        .await
        .with_context(|| format!("reading {url}"))?
        .to_vec())
}

/// Reads a manifest and checks it against the release key before any of
/// it is believed.
pub async fn fetch_release(client: &reqwest::Client, source: Source<'_>) -> Result<Release> {
    let url = source.url();
    let manifest = get(client, &url).await?;
    let signature = get(client, &format!("{url}.sig")).await?;
    let signature = String::from_utf8(signature).context("signature is not text")?;
    verify_signature(&manifest, &signature)?;
    let release: Release = serde_json::from_slice(&manifest)
        .with_context(|| format!("{url} is not a release manifest"))?;
    if let Source::Exact(version) = source {
        if release.version != version {
            bail!(
                "{url} describes {} rather than the requested {version}",
                release.version
            );
        }
    }
    Ok(release)
}

/// Downloads this host's artifact and returns it only once both its
/// signature and its digest agree with the manifest.
pub async fn download_verified(client: &reqwest::Client, release: &Release) -> Result<Vec<u8>> {
    let artifact = release.artifact_for_this_host()?;
    let url = format!("{RELEASE_BASE_URL}/{}", artifact.path);
    let binary = get(client, &url).await?;
    let signature = get(client, &format!("{url}.sig")).await?;
    let signature = String::from_utf8(signature).context("signature is not text")?;
    verify_signature(&binary, &signature)?;
    verify_digest(&binary, &artifact.sha256)?;
    Ok(binary)
}

/// Writes the new build beside the running one and renames it over the
/// top. The rename is atomic, and the running image stays mapped, so the
/// current process keeps working until it re-execs.
pub fn install_over_current_exe(binary: &[u8]) -> Result<PathBuf> {
    let exe = running_exe()?;
    let directory = exe
        .parent()
        .map(Path::to_path_buf)
        .unwrap_or_else(|| PathBuf::from("."));
    let staged = directory.join(format!(".pm-update-{}", std::process::id()));
    if let Err(error) = write_executable(&staged, binary) {
        let context = staging_context(&staged, &directory, error.kind());
        return Err(anyhow::Error::new(error).context(context));
    }
    if let Err(error) = std::fs::rename(&staged, &exe) {
        let _ = std::fs::remove_file(&staged);
        return Err(error).with_context(|| format!("replacing {}", exe.display()));
    }
    Ok(exe)
}

/// Why staging failed, in terms a reader of the log alone can act on.
///
/// An install writes beside the running binary, so the directory holding
/// it has to be writable by the account the service runs as. That is easy
/// to get wrong — a root-owned install directory under `User=` is the
/// usual shape — and the bare errno reads like a full disk or a bad
/// download, so a refusal on permissions names the directory it was
/// refused on.
fn staging_context(staged: &Path, directory: &Path, kind: std::io::ErrorKind) -> String {
    let staged = staged.display();
    if kind != std::io::ErrorKind::PermissionDenied {
        return format!("staging the new build at {staged}");
    }
    format!(
        "staging the new build at {staged}: installing replaces the running binary in place, \
         so {} has to be writable by uid {}, and it is not",
        directory.display(),
        // SAFETY: getuid is always successful and takes no arguments.
        unsafe { libc::getuid() },
    )
}

fn write_executable(path: &Path, bytes: &[u8]) -> std::io::Result<()> {
    use std::io::Write;
    use std::os::unix::fs::OpenOptionsExt;
    let mut file = std::fs::OpenOptions::new()
        .write(true)
        .create(true)
        .truncate(true)
        .mode(0o755)
        .open(path)?;
    file.write_all(bytes)?;
    file.sync_all()
}

/// The path of the binary this process is running, refusing a path that
/// no longer resolves. Installing works by renaming over the running
/// image, which unlinks its inode, and Linux then answers
/// `/proc/self/exe` with a `" (deleted)"` suffix. Renaming onto that name
/// would create a file called `pm (deleted)` and leave the real binary
/// untouched, so an exe path that does not exist is fatal here.
fn running_exe() -> Result<PathBuf> {
    resolve_exe(std::env::current_exe().context("locating the running pm binary")?)
}

fn resolve_exe(raw: PathBuf) -> Result<PathBuf> {
    let exe = std::fs::canonicalize(&raw).unwrap_or(raw);
    if !exe.exists() {
        bail!(
            "the running pm binary is no longer at {}, so it cannot be replaced in place",
            exe.display()
        );
    }
    Ok(exe)
}

/// Replaces this process with the build now at `exe`, keeping the
/// arguments it was started with. Only returns if the exec failed.
///
/// Takes the path the install resolved rather than asking for it again:
/// by this point the rename has unlinked the running image, so
/// `current_exe()` would answer with a `" (deleted)"` path that cannot be
/// exec'd.
pub fn reexec_current(exe: &Path) -> anyhow::Error {
    use std::os::unix::process::CommandExt;
    let error = std::process::Command::new(exe)
        .args(std::env::args_os().skip(1))
        .exec();
    anyhow::Error::new(error).context(format!(
        "restarting into the updated pm binary at {}",
        exe.display()
    ))
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The failure a service user hits on a root-owned install directory.
    /// Reading the log alone, a bare errno is indistinguishable from a
    /// full disk or a truncated download, so the refusal has to say what
    /// it could not write and why that matters.
    #[test]
    fn a_staging_refusal_on_permissions_names_the_directory_it_needs() {
        let context = staging_context(
            Path::new("/opt/pm/bin/.pm-update-221"),
            Path::new("/opt/pm/bin"),
            std::io::ErrorKind::PermissionDenied,
        );
        assert!(context.contains("/opt/pm/bin/.pm-update-221"), "{context}");
        assert!(context.contains("has to be writable"), "{context}");
        assert!(context.contains("/opt/pm/bin"), "{context}");
    }

    /// Every other cause keeps the short form: the errno already says it.
    #[test]
    fn a_staging_refusal_on_anything_else_stays_short() {
        let context = staging_context(
            Path::new("/opt/pm/bin/.pm-update-221"),
            Path::new("/opt/pm/bin"),
            std::io::ErrorKind::StorageFull,
        );
        assert_eq!(
            context,
            "staging the new build at /opt/pm/bin/.pm-update-221"
        );
    }

    /// The shape the sandbox produced: a directory the running user may
    /// not write reaches `write_executable` as `PermissionDenied`, which
    /// is what the message above keys off.
    #[test]
    fn staging_into_an_unwritable_directory_reports_permission_denied() {
        // SAFETY: getuid is always successful and takes no arguments.
        if unsafe { libc::getuid() } == 0 {
            return;
        }
        use std::os::unix::fs::PermissionsExt;
        let dir = tempfile::tempdir().expect("temp dir");
        std::fs::set_permissions(dir.path(), std::fs::Permissions::from_mode(0o555))
            .expect("drop write permission");
        let error = write_executable(&dir.path().join(".pm-update-1"), b"binary")
            .expect_err("a read-only directory accepted a write");
        assert_eq!(error.kind(), std::io::ErrorKind::PermissionDenied);
    }

    /// Set by the parent of the re-exec test to the directory its
    /// throwaway harness copy lives in.
    const REEXEC_DIR_VAR: &str = "PM_TEST_REEXEC_DIR";
    const REEXEC_CHILD: &str = "update::tests::the_child_half_of_the_re_exec_test";
    const REEXEC_MARKER: &str = "re-execed";

    /// Renaming over the running image unlinks its inode, and Linux then
    /// answers `/proc/self/exe` with a `" (deleted)"` suffix. Asking for
    /// the path again after the install is therefore how the re-exec dies,
    /// so the install and the re-exec have to be exercised together
    /// against a binary that really is replaced while it runs.
    ///
    /// The child does that to a throwaway copy of this harness and only
    /// its second image leaves the marker behind.
    #[test]
    fn a_replaced_binary_re_execs_into_its_installed_path() {
        let dir = tempfile::tempdir().unwrap();
        let harness = dir.path().join("harness");
        std::fs::copy(std::env::current_exe().unwrap(), &harness).unwrap();

        let status = std::process::Command::new(&harness)
            .args(["--exact", REEXEC_CHILD, "--ignored", "--nocapture"])
            .env(REEXEC_DIR_VAR, dir.path())
            .status()
            .unwrap();

        assert!(status.success(), "the child failed: {status}");
        assert!(
            dir.path().join(REEXEC_MARKER).exists(),
            "the child never reached its second image, so no re-exec happened"
        );
    }

    /// The child half of the test above. Ignored so an ordinary run skips
    /// it: it only means anything when it is a throwaway copy of the
    /// harness, since it replaces the binary it is running from.
    #[test]
    #[ignore]
    fn the_child_half_of_the_re_exec_test() {
        let Ok(dir) = std::env::var(REEXEC_DIR_VAR) else {
            return;
        };
        let marker = PathBuf::from(dir).join(REEXEC_MARKER);
        if marker.exists() {
            return;
        }
        let image = std::fs::read(running_exe().expect("the first image is on disk")).unwrap();
        let exe = install_over_current_exe(&image).expect("replacing the running harness");
        std::fs::write(&marker, b"").unwrap();
        panic!("re-exec failed: {:#}", reexec_current(&exe));
    }

    /// What a post-install `current_exe()` hands back on Linux. Renaming
    /// onto it would create a file literally called `pm (deleted)` and
    /// leave the real binary in place, so it has to be refused.
    #[test]
    fn an_exe_path_that_no_longer_exists_is_refused() {
        let dir = tempfile::tempdir().unwrap();
        let real = dir.path().join("pm");
        std::fs::write(&real, b"#!/bin/sh\n").unwrap();
        assert_eq!(
            resolve_exe(real.clone()).unwrap(),
            real.canonicalize().unwrap()
        );

        let vanished = dir.path().join("pm (deleted)");
        let error = resolve_exe(vanished).unwrap_err().to_string();
        assert!(error.contains("no longer at"), "unexpected error: {error}");
        assert!(
            !dir.path().join("pm (deleted)").exists(),
            "refusing the path must not create it"
        );
    }

    #[test]
    fn a_build_identity_reduces_to_its_release_version() {
        assert_eq!(release_version("0.2.0+abc1234"), "0.2.0");
        assert_eq!(release_version("0.2.0"), "0.2.0");
        assert_eq!(release_version(""), "");
    }

    #[test]
    fn upgrades_move_forward_only() {
        assert!(is_upgrade("0.1.0", "0.2.0"));
        assert!(is_upgrade("0.1.9", "0.2.0"));
        assert!(is_upgrade("0.9.0", "1.0.0"));
        assert!(!is_upgrade("0.2.0", "0.2.0"));
        assert!(!is_upgrade("0.2.0", "0.1.0"));
    }

    /// The semver precedence table, which is what lets a channel build
    /// lead to the stable it is heading for and sit below it.
    #[test]
    fn pre_releases_order_by_semver_precedence() {
        let ascending = [
            "0.9.17",
            "0.10.0-alpha",
            "0.10.0-alpha.1",
            "0.10.0-alpha.beta",
            "0.10.0-beta",
            "0.10.0-beta.2",
            "0.10.0-beta.11",
            "0.10.0-dev.3",
            "0.10.0-rc.1",
            "0.10.0",
            "0.10.1-dev.1",
            "0.10.1",
        ];
        for pair in ascending.windows(2) {
            assert!(is_upgrade(pair[0], pair[1]), "{} < {}", pair[0], pair[1]);
            assert!(!is_upgrade(pair[1], pair[0]), "{} > {}", pair[1], pair[0]);
        }
        assert!(!is_upgrade("0.10.0-dev.3", "0.10.0-dev.3"));
    }

    #[test]
    fn a_version_names_its_channel() {
        let dev = Version::parse("0.10.0-dev.3+abc1234").unwrap();
        assert!(dev.is_prerelease());
        assert_eq!(dev.channel(), Some("dev"));
        let stable = Version::parse("0.10.0").unwrap();
        assert!(!stable.is_prerelease());
        assert_eq!(stable.channel(), None);
        assert_eq!(Version::parse("0.10.0-7").unwrap().channel(), None);
    }

    #[test]
    fn malformed_pre_releases_do_not_parse() {
        for bad in [
            "0.10.0-",
            "0.10.0-dev..1",
            "0.10.0-dev.01",
            "0.10.0-dev.1.",
            "01.0.0",
            "0.10.0-dev_1",
        ] {
            assert!(Version::parse(bad).is_none(), "{bad:?} must not parse");
        }
    }

    #[test]
    fn channel_names_are_lowercase_words_and_never_the_stable_pointer() {
        for good in ["dev", "beta", "foo2", "rc"] {
            assert!(is_valid_channel_name(good), "{good}");
        }
        for bad in [
            "", "stable", "latest", "channels", "Dev", "dev-1", "2dev", "dev.1",
        ] {
            assert!(!is_valid_channel_name(bad), "{bad}");
        }
    }

    #[test]
    fn each_source_reads_its_own_manifest() {
        assert_eq!(
            Source::Stable.url(),
            format!("{RELEASE_BASE_URL}/latest.json")
        );
        assert_eq!(
            Source::Channel("dev").url(),
            format!("{RELEASE_BASE_URL}/channels/dev.json")
        );
        assert_eq!(
            Source::Exact("0.10.0-dev.3").url(),
            format!("{RELEASE_BASE_URL}/v0.10.0-dev.3/release.json")
        );
        assert_eq!(Source::for_channel(None), Source::Stable);
        assert_eq!(Source::for_channel(Some("stable")), Source::Stable);
        assert_eq!(Source::for_channel(Some("dev")), Source::Channel("dev"));
    }

    #[test]
    fn a_manifest_without_channel_fields_still_reads() {
        let release: Release =
            serde_json::from_str(r#"{"version":"0.9.17","protocolVersion":20,"artifacts":{}}"#)
                .unwrap();
        assert_eq!(release.channel, None);
        assert_eq!(release.promoted_from, None);
        let release: Release = serde_json::from_str(
            r#"{"version":"0.10.0","protocolVersion":20,"artifacts":{},"channel":null,"promotedFrom":"0.10.0-dev.3"}"#,
        )
        .unwrap();
        assert_eq!(release.promoted_from.as_deref(), Some("0.10.0-dev.3"));
    }

    /// A manifest that cannot be parsed must not be treated as newer, or
    /// serving junk would be enough to move a host off its build.
    #[test]
    fn an_unreadable_version_is_never_an_upgrade() {
        assert!(!is_upgrade("0.1.0", "banana"));
        assert!(!is_upgrade("banana", "0.2.0"));
        assert!(!is_upgrade("0.1.0", "0.2"));
        assert!(!is_upgrade("0.1.0", "0.2.0.1"));
        assert!(!is_upgrade("0.1.0", ""));
    }

    /// The build identity carries a git revision, which must not be
    /// mistaken for a version component.
    #[test]
    fn a_build_identity_compares_on_its_release_part() {
        assert!(is_upgrade("0.1.0+aaaaaaa", "0.2.0+bbbbbbb"));
        assert!(!is_upgrade("0.2.0+aaaaaaa", "0.2.0+bbbbbbb"));
    }

    #[test]
    fn digests_must_match_exactly() {
        let empty = "e3b0c44298fc1c149afbf4c8996fb92427ae41e4649b934ca495991b7852b855";
        assert!(verify_digest(b"", empty).is_ok());
        assert!(verify_digest(b"", &empty.to_uppercase()).is_ok());
        assert!(verify_digest(b"tampered", empty).is_err());
    }

    /// Signing is what the release script does; verifying is what every
    /// installed binary does. Exercising both ends together is the only
    /// way to know the two agree on the format.
    fn signing_key() -> (ring::signature::Ed25519KeyPair, String) {
        use ring::signature::KeyPair;
        let rng = ring::rand::SystemRandom::new();
        let pkcs8 = ring::signature::Ed25519KeyPair::generate_pkcs8(&rng).unwrap();
        let pair = ring::signature::Ed25519KeyPair::from_pkcs8(pkcs8.as_ref()).unwrap();
        let public = base64::engine::general_purpose::STANDARD.encode(pair.public_key().as_ref());
        (pair, public)
    }

    #[test]
    fn a_signature_from_the_release_key_verifies() {
        let (pair, public) = signing_key();
        let payload = b"the pm binary";
        let signature = base64::engine::general_purpose::STANDARD.encode(pair.sign(payload));
        assert!(verify_with(&public, payload, &signature).is_ok());
    }

    #[test]
    fn a_tampered_artifact_fails_its_signature() {
        let (pair, public) = signing_key();
        let signature =
            base64::engine::general_purpose::STANDARD.encode(pair.sign(b"the pm binary"));
        assert!(verify_with(&public, b"a different binary", &signature).is_err());
    }

    /// A signature made by some other key must not pass, or publishing
    /// would only require write access to the bucket.
    #[test]
    fn a_signature_from_another_key_is_rejected() {
        let (_, public) = signing_key();
        let (attacker, _) = signing_key();
        let payload = b"the pm binary";
        let signature = base64::engine::general_purpose::STANDARD.encode(attacker.sign(payload));
        assert!(verify_with(&public, payload, &signature).is_err());
    }

    #[test]
    fn malformed_signatures_and_keys_are_rejected() {
        let (pair, public) = signing_key();
        let signature = base64::engine::general_purpose::STANDARD.encode(pair.sign(b"x"));
        assert!(verify_with(&public, b"x", "not base64!!").is_err());
        assert!(verify_with("not base64!!", b"x", &signature).is_err());
        assert!(verify_with(&public, b"x", "").is_err());
    }

    /// Without a release key the only safe answer is to refuse, rather
    /// than accept whatever the transport delivered.
    #[test]
    fn a_build_without_a_release_key_refuses_to_verify() {
        if updates_enabled() {
            return;
        }
        let error = verify_signature(b"payload", "AAAA").unwrap_err();
        assert!(
            error.to_string().contains("updates are disabled"),
            "{error}"
        );
    }

    /// The release script signs with Node and this verifies with ring, so
    /// the two have to agree on the encoding. This vector was produced by
    /// scripts/publish.mjs; if it ever stops verifying, the publish side
    /// and the install side have drifted apart.
    #[test]
    fn a_signature_produced_by_the_release_script_verifies() {
        const PUBLIC_KEY: &str = "f1gyamoUyD4JqWRdHhWxI7q+DFq/F+GgBAz+SigODwI=";
        const SIGNATURE: &str =
            "WdijG5Gc2w8+Qljs0wFFijJ9soDDRHEt0RhTWcUhY7ikMTP72K+q8jFO5r3u9viKWo3RD1Twvx2eL3TpGtEdCg==";
        assert!(verify_with(PUBLIC_KEY, b"puppet-master release vector", SIGNATURE).is_ok());
        assert!(verify_with(PUBLIC_KEY, b"something else", SIGNATURE).is_err());
    }

    #[test]
    fn this_host_names_a_published_target() {
        assert_ne!(
            target_triple(),
            "unsupported",
            "tests run on a target the release process publishes"
        );
    }
}

/// The newest published release, refreshed in the background so the
/// daemon can say a build is available without checking on demand.
#[derive(Default)]
pub struct LatestRelease(std::sync::Mutex<Option<Release>>);

impl LatestRelease {
    pub fn get(&self) -> Option<Release> {
        self.0.lock().unwrap().clone()
    }

    fn set(&self, release: Release) {
        *self.0.lock().unwrap() = Some(release);
    }

    /// Whether a newer build than this one has been published.
    pub fn upgrade_available(&self) -> Option<String> {
        let release = self.get()?;
        is_upgrade(pm_build_version(), &release.version).then_some(release.version)
    }
}

/// Polls for the newest release on the host's channel. The daemon only
/// reports what it finds: replacing a running daemon would end every
/// agent session it holds, so applying an update stays an explicit act.
pub fn watch_latest(
    latest: std::sync::Arc<LatestRelease>,
    channel: Option<String>,
    every: std::time::Duration,
) {
    if !updates_enabled() {
        return;
    }
    tokio::spawn(async move {
        let source = Source::for_channel(channel.as_deref());
        let Ok(client) = reqwest::Client::builder()
            .user_agent(format!("pm/{}", pm_build_version()))
            .build()
        else {
            return;
        };
        loop {
            match fetch_release(&client, source).await {
                Ok(release) => latest.set(release),
                Err(error) => tracing::debug!(%error, "could not read the release manifest"),
            }
            tokio::time::sleep(every).await;
        }
    });
}
