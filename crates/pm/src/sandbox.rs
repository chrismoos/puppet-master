//! `pm worker --sandbox`: launches a container whose entrypoint is the
//! ordinary `pm worker`. This module never becomes a worker itself — it
//! resolves the image, builds the container invocation, and hands over.
//! The image carries a Linux pm binary, so the launcher works even when
//! it is a macOS executable. On docker and podman that image is the
//! release's published one by default, pulled on first use, and
//! [`LOCAL_IMAGE`] is the one `make sandbox-image` builds from source.

use std::net::{IpAddr, SocketAddr, ToSocketAddrs};
use std::path::{Path, PathBuf};
use std::process::Command;
use std::time::Duration;

use anyhow::{anyhow, bail, Context};

/// Fixed path of the Linux pm binary built into the container image.
pub const CONTAINER_BIN: &str = "/opt/pm/bin/pm";
/// Subtree reserved for the image's pm binary; user mounts must not touch it.
const CONTAINER_BIN_DIR: &str = "/opt/pm";
/// The image's worker home. A named volume mounts here so the worker's
/// controller enrollment survives container replacement.
pub const CONTAINER_HOME: &str = "/home/worker";
/// Repository the release publishes the worker image to. A macro
/// because the tag below is built with `concat!`, which takes only
/// literals. `scripts/publish.mjs` pushes the same name, and a test
/// reads both so the launcher cannot default to an image no release
/// pushes.
macro_rules! image_repo {
    () => {
        "ghcr.io/chrismoos/puppet-master-worker"
    };
}
/// Published image this launcher pairs with, tagged with its own version
/// rather than `latest`: the launcher refuses an image whose pm reports
/// a different version, so a floating tag would break every launcher but
/// the newest.
pub const DEFAULT_IMAGE: &str = concat!(image_repo!(), ":", env!("CARGO_PKG_VERSION"));
/// Local tag `make sandbox-image` builds from the working tree. It is
/// what a build ahead of a release has to run, since no published image
/// carries that version yet.
pub const LOCAL_IMAGE: &str = "puppet-master-worker:local";
/// Stock system-container image the Incus runtime provisions at launch.
pub const DEFAULT_INCUS_IMAGE: &str = "images:ubuntu/26.04";
/// Set in the container environment by the launcher; `--sandbox` is
/// rejected when it is present so a sandboxed worker cannot re-sandbox.
pub const INNER_MARKER: &str = "PM_SANDBOX_INNER";
/// Names the runtime that launched the container, which the worker
/// reports at registration so the Hosts page can say what a host is.
/// Nothing on the worker plane can reach the runtime to ask.
pub const RUNTIME_MARKER: &str = "PM_SANDBOX_RUNTIME";
/// The runtime's own name for the container, reported alongside
/// [`RUNTIME_MARKER`] because it is what an operator has to type.
pub const CONTAINER_MARKER: &str = "PM_SANDBOX_CONTAINER";

const IMAGE_BUILD_HINT: &str = "make sandbox-image";
const SNAPSHOT_TIMEOUT: Duration = Duration::from_secs(2);

/// Suffix of the volume holding CONTAINER_HOME.
const HOME_VOLUME_SUFFIX: &str = "-home";
/// Incus instance and volume names are hostnames: 63 characters of
/// letters, digits and hyphens. The home volume appends its suffix to the
/// instance name, so the instance name has to leave room for it.
const INCUS_NAME_MAX: usize = 63 - HOME_VOLUME_SUFFIX.len();
/// Storage pool used when the default profile names none.
const INCUS_FALLBACK_POOL: &str = "default";
const HOME_DEVICE: &str = "home";
const LISTEN_DEVICE: &str = "pm-listen";
/// The instance's own NIC, which `--network` repoints at another Incus network.
const NIC_DEVICE: &str = "eth0";
/// The instance's root disk, whose size `--disk` caps.
const ROOT_DEVICE: &str = "root";
/// Unit the provisioned container runs the worker under. systemd stays
/// PID 1, which is what lets a container runtime nest inside.
const WORKER_UNIT: &str = "pm-worker.service";
const WORKER_USER: &str = "worker";
const WORKER_UID: u32 = 1000;
/// Drop-in granting WORKER_USER passwordless root inside the container.
/// The name carries no dot, because sudo skips every file in this
/// directory whose name has one.
const WORKER_SUDOERS_FILE: &str = "/etc/sudoers.d/pm-worker";
/// Where the drop-in is written before `visudo` accepts it. It is staged
/// outside `/etc/sudoers.d` so a file sudo would read never exists in an
/// unchecked state, and in `/etc` rather than a world-writable directory.
const WORKER_SUDOERS_STAGE: &str = "/etc/pm-worker.sudoers.new";
/// Distribution packages the provisioned container needs.
const PROVISION_PACKAGES: &[&str] = &[
    "ca-certificates",
    "curl",
    "git",
    "nodejs",
    "npm",
    "openssh-client",
    "ripgrep",
    "sudo",
];
const PROVISION_NPM_PACKAGES: &[&str] = &["@anthropic-ai/claude-code", "@openai/codex"];
/// Seconds provisioning waits for the container's resolver. A fresh
/// instance answers `incus exec` before systemd-resolved is up, so the
/// first name lookup fails unless it is gated.
const RESOLVER_WAIT_SECS: u32 = 60;
/// systemd is PID 1 in an Incus container, so the worker has to be found
/// by name before its terminal children can be counted.
const LIVE_TERMINALS_PROBE: &str = "p=$(pgrep -x pm | head -1); cat /proc/$p/task/$p/children";

/// What the runtime does when the container stops. The runtime is the
/// only supervisor a sandboxed worker has, which is why the default
/// supervises: `unless-stopped` brings the worker back from a crash or a
/// host reboot while still respecting a deliberate stop.
#[derive(
    Debug,
    Clone,
    Copy,
    Default,
    PartialEq,
    Eq,
    clap::ValueEnum,
    serde::Serialize,
    serde::Deserialize,
)]
#[serde(rename_all = "kebab-case")]
pub enum RestartPolicy {
    No,
    OnFailure,
    Always,
    #[default]
    UnlessStopped,
}

impl SandboxLimits {
    /// True when no cap is set, which is what a container gets by
    /// default and so needs no record.
    pub fn is_unset(&self) -> bool {
        *self == Self::default()
    }
}

impl RestartPolicy {
    /// The policy as an operator writes it, for listings and messages.
    pub fn as_word(self) -> &'static str {
        self.as_runtime_arg()
    }

    fn as_runtime_arg(self) -> &'static str {
        match self {
            RestartPolicy::No => "no",
            RestartPolicy::OnFailure => "on-failure",
            RestartPolicy::Always => "always",
            RestartPolicy::UnlessStopped => "unless-stopped",
        }
    }

    /// Incus `boot.autostart`, or None to leave it unset. Unset means
    /// last-state — Incus restores whatever was running — which is what
    /// unless-stopped means.
    fn as_autostart(self) -> Option<&'static str> {
        match self {
            RestartPolicy::No | RestartPolicy::OnFailure => Some("false"),
            RestartPolicy::Always => Some("true"),
            RestartPolicy::UnlessStopped => None,
        }
    }

    /// The unit's `Restart=`. Under Incus two layers supervise: systemd
    /// restarts the process, `boot.autostart` restores the instance.
    fn as_systemd_restart(self) -> &'static str {
        match self {
            RestartPolicy::No => "no",
            RestartPolicy::OnFailure => "on-failure",
            RestartPolicy::Always | RestartPolicy::UnlessStopped => "always",
        }
    }
}

#[derive(
    Debug, Clone, Copy, PartialEq, Eq, clap::ValueEnum, serde::Serialize, serde::Deserialize,
)]
#[serde(rename_all = "kebab-case")]
pub enum MemoryEnforce {
    Hard,
    Soft,
}

impl MemoryEnforce {
    fn as_incus_value(self) -> &'static str {
        match self {
            MemoryEnforce::Hard => "hard",
            MemoryEnforce::Soft => "soft",
        }
    }
}

/// Resource caps for the sandbox. Every one is optional, and unset means
/// unlimited, which is what a container gets by default on both runtimes.
#[derive(Debug, Clone, Default, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct SandboxLimits {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub cpu: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub memory: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub memory_swap: Option<bool>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub cpu_allowance: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub memory_enforce: Option<MemoryEnforce>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub disk: Option<String>,
}

impl SandboxLimits {
    /// Caps Incus can apply and the OCI runtimes cannot, named as the
    /// flags that set them.
    fn incus_only(&self) -> Vec<&'static str> {
        let mut flags = Vec::new();
        if self.cpu_allowance.is_some() {
            flags.push("--cpu-allowance");
        }
        if self.memory_enforce.is_some() {
            flags.push("--memory-enforce");
        }
        if self.disk.is_some() {
            flags.push("--disk");
        }
        flags
    }

    fn incus_config(&self) -> Vec<String> {
        let mut config = Vec::new();
        if let Some(cpu) = &self.cpu {
            config.push(format!("limits.cpu={cpu}"));
        }
        if let Some(memory) = &self.memory {
            config.push(format!("limits.memory={memory}"));
        }
        if let Some(swap) = self.memory_swap {
            config.push(format!("limits.memory.swap={swap}"));
        }
        if let Some(allowance) = &self.cpu_allowance {
            config.push(format!("limits.cpu.allowance={allowance}"));
        }
        if let Some(enforce) = self.memory_enforce {
            config.push(format!(
                "limits.memory.enforce={}",
                enforce.as_incus_value()
            ));
        }
        config
    }

    /// Denying swap on an OCI runtime needs a memory cap to deny it
    /// against, because Docker caps memory and swap as one total.
    fn oci_swap_needs_memory(&self) -> bool {
        self.memory_swap == Some(false) && self.memory.is_none()
    }

    /// The same caps as OCI run arguments. Docker spells swap as a total
    /// size rather than a switch: unlimited is -1, and denying swap means
    /// setting the total to the memory cap.
    fn oci_args(&self) -> Vec<String> {
        let mut args = Vec::new();
        if let Some(cpu) = &self.cpu {
            args.push("--cpus".to_string());
            args.push(cpu.clone());
        }
        if let Some(memory) = &self.memory {
            args.push("--memory".to_string());
            args.push(memory.clone());
        }
        // Validation has already refused a denied swap with no memory cap.
        if let Some(total) = match (self.memory_swap, &self.memory) {
            (Some(true), _) => Some("-1".to_string()),
            (Some(false), memory) => memory.clone(),
            (None, _) => None,
        } {
            args.push("--memory-swap".to_string());
            args.push(total);
        }
        args
    }
}

#[cfg(test)]
const NO_LIMITS: SandboxLimits = SandboxLimits {
    cpu: None,
    memory: None,
    memory_swap: None,
    cpu_allowance: None,
    memory_enforce: None,
    disk: None,
};

#[derive(
    Debug, Clone, Copy, PartialEq, Eq, clap::ValueEnum, serde::Serialize, serde::Deserialize,
)]
#[serde(rename_all = "kebab-case")]
pub enum RuntimeKind {
    Docker,
    Podman,
    Incus,
}

impl RuntimeKind {
    pub fn command(self) -> &'static str {
        match self {
            RuntimeKind::Docker => "docker",
            RuntimeKind::Podman => "podman",
            RuntimeKind::Incus => "incus",
        }
    }
}

#[derive(Clone)]
pub struct SandboxArgs {
    /// The locally configured worker this container belongs to, which
    /// names the container and its home volume.
    pub profile: String,
    pub controller: Option<String>,
    pub token: Option<String>,
    pub listen: Option<SocketAddr>,
    pub listen_any: bool,
    pub allow_from: Vec<String>,
    pub dirs: Vec<String>,
    pub restart: RestartPolicy,
    /// Stay attached to the container's output instead of returning once
    /// it is up. Detaching is the default: the runtime supervises the
    /// worker, so there is nothing for a terminal to hold open.
    pub foreground: bool,
    pub image: Option<String>,
    pub env: Vec<String>,
    pub network: Option<String>,
    pub runtime: Option<RuntimeKind>,
    pub incus_shifted_home: bool,
    pub force: bool,
    /// Rebuild the container even when one already exists and could
    /// simply be started, which is what applying changed settings
    /// requires.
    pub rebuild: bool,
    pub limits: SandboxLimits,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct MountSpec {
    pub host: PathBuf,
    pub container: PathBuf,
    pub read_only: bool,
}

impl MountSpec {
    fn volume_arg(&self) -> String {
        format!(
            "{}:{}:{}",
            self.host.display(),
            self.container.display(),
            if self.read_only { "ro" } else { "rw" }
        )
    }
}

/// Parses `HOST[:CONTAINER][:ro|rw]`. A bare host path mounts at the
/// identical container path, which is what remote spawns need: the
/// controller launches sessions at the project's configured path
/// verbatim, so that path must exist in the container as-is.
fn parse_mount_spec(spec: &str) -> anyhow::Result<MountSpec> {
    let parts: Vec<&str> = spec.split(':').collect();
    let (host, container, mode) = match parts.as_slice() {
        [host] => (*host, *host, None),
        [host, mode @ ("ro" | "rw")] => (*host, *host, Some(*mode)),
        [host, container] => (*host, *container, None),
        [host, container, mode] => (*host, *container, Some(*mode)),
        _ => bail!("invalid --dir {spec:?}: expected HOST[:CONTAINER][:ro|rw]"),
    };
    let read_only = match mode {
        None => false,
        Some("ro") => true,
        Some("rw") => false,
        Some(other) => bail!("invalid --dir mode {other:?} in {spec:?}: expected ro or rw"),
    };
    let host = Path::new(host);
    let container = Path::new(container);
    if !host.is_absolute() || !container.is_absolute() {
        bail!("invalid --dir {spec:?}: both paths must be absolute");
    }
    for protected in ["/usr", "/etc", CONTAINER_HOME] {
        if Path::new(protected).starts_with(container) {
            bail!(
                "refusing --dir {spec:?}: mounting at {} would shadow the image's {protected}",
                container.display()
            );
        }
    }
    let bin_dir = Path::new(CONTAINER_BIN_DIR);
    if container.starts_with(bin_dir) || bin_dir.starts_with(container) {
        bail!(
            "refusing --dir {spec:?}: {} is reserved for the image's pm binary",
            CONTAINER_BIN_DIR
        );
    }
    Ok(MountSpec {
        host: host.to_path_buf(),
        container: container.to_path_buf(),
        read_only,
    })
}

/// Mirrors the worker's controller normalization so the launcher and
/// the inner worker agree on the same controller key.
fn normalize_controller(controller: &str) -> String {
    let base = controller.trim_end_matches('/');
    base.strip_prefix("http://")
        .map(|r| format!("ws://{r}"))
        .or_else(|| base.strip_prefix("https://").map(|r| format!("wss://{r}")))
        .unwrap_or_else(|| base.to_string())
}

fn controller_endpoint(controller: &str) -> anyhow::Result<(String, u16)> {
    let normalized = normalize_controller(controller);
    let (rest, default_port) = normalized
        .strip_prefix("ws://")
        .map(|r| (r, 80))
        .or_else(|| normalized.strip_prefix("wss://").map(|r| (r, 443)))
        .ok_or_else(|| {
            anyhow!("invalid controller URL {controller:?}: expected ws(s):// or http(s)://")
        })?;
    let authority = rest.split('/').next().unwrap_or_default();
    if authority.is_empty() {
        bail!("invalid controller URL {controller:?}: no host");
    }
    if let Some(bracketed) = authority.strip_prefix('[') {
        let (host, tail) = bracketed.split_once(']').ok_or_else(|| {
            anyhow!("invalid controller URL {controller:?}: unterminated IPv6 host")
        })?;
        let port = match tail.strip_prefix(':') {
            Some(p) => p
                .parse()
                .map_err(|_| anyhow!("invalid controller port {p:?} in {controller:?}"))?,
            None => default_port,
        };
        return Ok((host.to_string(), port));
    }
    match authority.rsplit_once(':') {
        Some((host, port)) => {
            let port = port
                .parse()
                .map_err(|_| anyhow!("invalid controller port {port:?} in {controller:?}"))?;
            Ok((host.to_string(), port))
        }
        None => Ok((authority.to_string(), default_port)),
    }
}

/// Resolves the controller only for the direction that needs one. A worker the
/// controller dials must not inherit an unrelated saved controller merely so
/// the sandbox launcher can derive its name.
fn sandbox_controller(
    controller: Option<&str>,
    listen: Option<SocketAddr>,
    profile: &str,
) -> anyhow::Result<Option<String>> {
    if listen.is_some() {
        return Ok(None);
    }
    controller
        .map(normalize_controller)
        .or_else(|| crate::worker::stored_controller(profile))
        .map(Some)
        .ok_or_else(|| {
            anyhow!("worker {profile} has no controller saved: pass --controller to enroll it")
        })
}

fn validate_listener_options(
    listen: Option<SocketAddr>,
    listen_any: bool,
    allow_from: &[String],
) -> anyhow::Result<()> {
    let Some(listen) = listen else {
        return Ok(());
    };
    if listen.ip().is_unspecified() && !listen_any {
        bail!(
            "refusing to publish {} on every host interface: pass --listen-any to mean it",
            listen
        );
    }
    for cidr in allow_from {
        crate::worker_listener::Cidr::parse(cidr)?;
    }
    Ok(())
}

/// Deterministic container name, so relaunching replaces the previous
/// container instead of accumulating duplicates.
/// The container holding a named worker, for callers outside the
/// launcher that have to name it.
pub fn container_name_of(profile: &str) -> String {
    container_name(profile)
}

/// The container holding a named worker. Derived from the name alone so
/// it is the same on every launch and can be printed, which is what
/// lets an operator reach the container at all.
fn container_name(profile: &str) -> String {
    let sanitized: String = profile
        .chars()
        .map(|c| {
            if c.is_ascii_alphanumeric() || matches!(c, '_' | '.' | '-') {
                c
            } else {
                '-'
            }
        })
        .collect();
    format!("pm-worker-{sanitized}")
}

fn home_volume_name(container_name: &str) -> String {
    format!("{container_name}{HOME_VOLUME_SUFFIX}")
}

/// The instance name Incus will accept for a container named `base`.
fn incus_instance_name(base: &str) -> String {
    let mut name: String = base
        .chars()
        .map(|c| if c.is_ascii_alphanumeric() { c } else { '-' })
        .collect();
    name.truncate(INCUS_NAME_MAX);
    while name.ends_with('-') {
        name.pop();
    }
    name
}

/// Project paths no mount's container side covers. A spawn into an
/// uncovered path fails to chdir inside the container.
fn uncovered_paths(project_paths: &[String], mounts: &[MountSpec]) -> Vec<String> {
    let mut uncovered: Vec<String> = project_paths
        .iter()
        .filter(|p| !p.trim().is_empty())
        .filter(|p| {
            let path = Path::new(p.as_str());
            !mounts.iter().any(|m| path.starts_with(&m.container))
        })
        .cloned()
        .collect();
    uncovered.sort();
    uncovered.dedup();
    uncovered
}

pub fn refuse_re_sandbox(inner_marker_set: bool) -> anyhow::Result<()> {
    if inner_marker_set {
        bail!("already running inside a sandboxed worker; --sandbox cannot nest");
    }
    Ok(())
}

struct LaunchSpec<'a> {
    name: &'a str,
    /// What the worker inside reports as its runtime, which is also the
    /// command an operator runs to manage the container.
    runtime_name: &'a str,
    image: &'a str,
    mounts: &'a [MountSpec],
    restart: RestartPolicy,
    foreground: bool,
    /// Digest of the settings this container is being built from,
    /// recorded on it so a later launch can tell the profile has moved.
    spec: &'a str,
    /// The locally configured worker, named in what a launch prints so
    /// the `pm worker` commands it suggests can be copied as they are.
    profile: &'a str,
    network: Option<&'a str>,
    env: &'a [String],
    host_entry: Option<(&'a str, IpAddr)>,
    /// A controller name the launcher could not resolve, mapped to the
    /// container's gateway instead. `host.docker.internal` only exists
    /// inside a container on Docker Desktop, so on Linux neither end
    /// resolves it and the worker never reaches the controller.
    gateway_host: Option<&'a str>,
    controller: Option<&'a str>,
    token: Option<&'a str>,
    /// Set when the controller dials this host instead. Bridge networking
    /// publishes the port; host networking binds the enrolled address.
    listen: Option<SocketAddr>,
    allow_from: &'a [String],
    limits: &'a SandboxLimits,
    /// Instance idmap, when the launching user needs mapping onto the
    /// worker's uid. None leaves the instance's default mapping.
    idmap: Option<String>,
    incus_shifted_home: bool,
}

fn publish_arg(listen: SocketAddr) -> String {
    match listen.ip() {
        IpAddr::V4(ip) => format!("{ip}:{}:{}", listen.port(), listen.port()),
        IpAddr::V6(ip) => format!("[{ip}]:{}:{}", listen.port(), listen.port()),
    }
}

fn build_run_args(spec: &LaunchSpec) -> Vec<String> {
    let limit_args = spec.limits.oci_args();
    let mut args: Vec<String> = vec!["run".into()];
    if !spec.foreground {
        args.push("--detach".into());
    }
    args.push("--label".into());
    args.push(format!("{SPEC_LABEL}={}", spec.spec));
    args.push("--name".into());
    args.push(spec.name.into());
    if spec.restart != RestartPolicy::No {
        args.push("--restart".into());
        args.push(spec.restart.as_runtime_arg().into());
    }
    args.push("--env".into());
    args.push(format!("{INNER_MARKER}=1"));
    args.push("--env".into());
    args.push(format!("{RUNTIME_MARKER}={}", spec.runtime_name));
    args.push("--env".into());
    args.push(format!("{CONTAINER_MARKER}={}", spec.name));
    if let Some((host, ip)) = spec.host_entry {
        args.push("--add-host".into());
        args.push(format!("{host}:{ip}"));
    } else if let Some(host) = spec.gateway_host {
        args.push("--add-host".into());
        args.push(format!("{host}:{HOST_GATEWAY}"));
    }
    if let Some(network) = spec.network {
        args.push("--network".into());
        args.push(network.into());
    }
    if let Some(listen) = spec.listen.filter(|_| spec.network != Some("host")) {
        // Published on the host at the same port the container binds, so the
        // address an operator enrolled is the address that answers.
        args.push("--publish".into());
        args.push(publish_arg(listen));
    }
    args.push("--volume".into());
    args.push(format!("{}:{CONTAINER_HOME}", home_volume_name(spec.name)));
    for mount in spec.mounts {
        args.push("--volume".into());
        args.push(mount.volume_arg());
    }
    for env in spec.env {
        args.push("--env".into());
        args.push(env.clone());
    }
    args.extend(limit_args);
    args.push(spec.image.into());
    args.push(CONTAINER_BIN.into());
    args.extend(worker_command_args(spec));
    args
}

/// The worker invocation itself, which every runtime hands the same
/// arguments however it starts the process.
fn worker_command_args(spec: &LaunchSpec) -> Vec<String> {
    let mut args: Vec<String> = vec!["worker".into()];
    match spec.listen {
        // A dialed host has no controller URL. A bridged container binds all
        // of its own interfaces and relies on the publish to narrow exposure;
        // a host-networked container can bind the enrolled address itself.
        Some(listen) => {
            args.push("--listen".into());
            if spec.network == Some("host") {
                args.push(listen.to_string());
                if listen.ip().is_unspecified() {
                    args.push("--listen-any".into());
                }
            } else {
                args.push(format!("0.0.0.0:{}", listen.port()));
                args.push("--listen-any".into());
            }
            for cidr in spec.allow_from {
                args.push("--allow-from".into());
                args.push(cidr.clone());
            }
        }
        None => {
            args.push("--controller".into());
            args.push(
                spec.controller
                    .expect("dialing sandbox needs a controller")
                    .into(),
            );
        }
    }
    if let Some(token) = spec.token {
        args.push("--token".into());
        args.push(token.into());
    }
    args
}

/// `KEY=VALUE` as given, or a bare `KEY` resolved from the launcher's own
/// environment. An unset bare key is dropped, matching what the OCI
/// runtimes do with one.
fn resolved_env(env: &[String]) -> Vec<(String, String)> {
    env.iter()
        .filter_map(|entry| match entry.split_once('=') {
            Some((key, value)) => Some((key.to_string(), value.to_string())),
            None => std::env::var(entry)
                .ok()
                .map(|value| (entry.clone(), value)),
        })
        .collect()
}

/// Runtimes tried in order. Incus comes first where it exists, because
/// its system container runs systemd as PID 1 and a container runtime
/// nests inside it, which is what lets an agent build and run
/// containers. Incus has no macOS host support, so that platform has the
/// OCI runtimes and nothing else.
fn detection_order(linux: bool) -> &'static [RuntimeKind] {
    const LINUX: [RuntimeKind; 3] = [RuntimeKind::Incus, RuntimeKind::Docker, RuntimeKind::Podman];
    const ELSEWHERE: [RuntimeKind; 2] = [RuntimeKind::Docker, RuntimeKind::Podman];
    if linux {
        &LINUX
    } else {
        &ELSEWHERE
    }
}

fn runtime_available(runtime: RuntimeKind) -> bool {
    // The Incus client answers --version with no daemon behind it, and a
    // launcher that cannot reach the daemon can do nothing, so this asks
    // the daemon something instead.
    let probe: &[&str] = match runtime {
        RuntimeKind::Incus => &["storage", "list"],
        RuntimeKind::Docker | RuntimeKind::Podman => &["--version"],
    };
    Command::new(runtime.command())
        .args(probe)
        .output()
        .map(|o| o.status.success())
        .unwrap_or(false)
}

fn detect_runtime() -> anyhow::Result<RuntimeKind> {
    let order = detection_order(cfg!(target_os = "linux"));
    for runtime in order {
        if runtime_available(*runtime) {
            return Ok(*runtime);
        }
    }
    bail!(
        "no container runtime found: install one of {}, or pass --runtime",
        order
            .iter()
            .map(|r| r.command())
            .collect::<Vec<_>>()
            .join(", ")
    )
}

fn runtime_output(runtime: RuntimeKind, args: &[&str]) -> anyhow::Result<std::process::Output> {
    Command::new(runtime.command())
        .args(args)
        .output()
        .with_context(|| format!("running {} {}", runtime.command(), args.join(" ")))
}

fn image_exists(runtime: RuntimeKind, image: &str) -> bool {
    runtime_output(runtime, &["image", "inspect", image])
        .map(|o| o.status.success())
        .unwrap_or(false)
}

/// What to do about an image the runtime does not have.
#[derive(Debug, PartialEq, Eq)]
enum MissingImage {
    /// The release publishes it under this exact tag.
    Pull,
    /// Only the working tree can produce it.
    Build,
    /// Named with `--image`, so its origin is the operator's to know.
    Unknown,
}

fn missing_image_action(image: &str) -> MissingImage {
    match image {
        DEFAULT_IMAGE => MissingImage::Pull,
        LOCAL_IMAGE => MissingImage::Build,
        _ => MissingImage::Unknown,
    }
}

/// Fetches the published image, leaving the runtime's progress on the
/// terminal: it is a few hundred megabytes over the network, and a
/// silent first launch reads as a hang.
fn pull_image(runtime: RuntimeKind, image: &str) -> anyhow::Result<()> {
    let command = runtime.command();
    eprintln!("pulling {image}");
    let status = Command::new(command)
        .args(["pull", image])
        .status()
        .with_context(|| format!("running {command} pull"))?;
    if status.success() {
        return Ok(());
    }
    bail!(
        "{command} pull {image} failed. Two usual causes:\n  \
         the image is published but its package is private, so an anonymous pull is \
         refused. Make the package public once in its registry settings.\n  \
         this pm is built from source ahead of any release, so no published tag carries \
         its version. Build an image from this tree with `{IMAGE_BUILD_HINT}` and pass \
         `--image {LOCAL_IMAGE}`."
    );
}

/// Running state of the named container, or None when it does not exist.
fn container_running(runtime: RuntimeKind, name: &str) -> Option<bool> {
    let out = runtime_output(
        runtime,
        &["inspect", "--format", "{{.State.Running}}", name],
    )
    .ok()?;
    if !out.status.success() {
        return None;
    }
    Some(String::from_utf8_lossy(&out.stdout).trim() == "true")
}

/// Live terminals in the container, counted as direct children of the
/// worker process (PID 1): each agent session or shell owns one PTY
/// child. None when the count cannot be determined — the children
/// listing needs CONFIG_PROC_CHILDREN, which not every kernel has, and
/// exec itself can fail on a live container.
fn live_terminal_count(runtime: RuntimeKind, name: &str) -> Option<usize> {
    let out = runtime_output(runtime, &["exec", name, "cat", "/proc/1/task/1/children"]).ok()?;
    if !out.status.success() {
        return None;
    }
    Some(count_children(&String::from_utf8_lossy(&out.stdout)))
}

fn count_children(children: &str) -> usize {
    children.split_whitespace().count()
}

/// Label recording which settings a container was built from, so a
/// later launch can tell that the profile has moved on without having
/// to compare every runtime argument back out of the runtime.
const SPEC_LABEL: &str = "pm.spec";
/// What docker and podman call the address a container reaches its host
/// at. Resolving it is the runtime's job, so it is passed as written.
const HOST_GATEWAY: &str = "host-gateway";

/// A short digest of everything that decides how the container is built.
/// Only equality matters, so the shortest thing that changes when the
/// settings change will do.
fn spec_fingerprint(args: &SandboxArgs, image: &str, mounts: &[MountSpec]) -> String {
    let mut parts: Vec<String> = vec![
        image.to_string(),
        args.restart.as_runtime_arg().to_string(),
        args.network.clone().unwrap_or_default(),
    ];
    if args.incus_shifted_home {
        parts.push("incus-shifted-home=true".into());
    }
    parts.extend(mounts.iter().map(|m| m.volume_arg()));
    parts.extend(args.env.iter().cloned());
    parts.extend(args.limits.oci_args());
    let joined = parts.join("\u{1f}");
    format!("{:016x}", fnv1a(joined.as_bytes()))
}

/// FNV-1a, which is enough to notice a changed setting and avoids
/// pulling in a hash crate for a label nothing security-sensitive reads.
fn fnv1a(bytes: &[u8]) -> u64 {
    let mut hash: u64 = 0xcbf2_9ce4_8422_2325;
    for byte in bytes {
        hash ^= u64::from(*byte);
        hash = hash.wrapping_mul(0x0000_0100_0000_01b3);
    }
    hash
}

/// What a launch prints once the container is up. The runtime supervises
/// the worker from here, so this is where an operator learns the name
/// they need and the three commands they will actually want.
fn launch_report(runtime: &str, name: &str, profile: &str, restart: RestartPolicy) -> String {
    let mut out = format!("{name} is running in {runtime}.\n\n");
    out.push_str(&format!("  logs     {runtime} logs -f {name}\n"));
    out.push_str(&format!("  stop     {runtime} stop {name}\n"));
    out.push_str(&format!("  start    {runtime} start {name}\n"));
    out.push_str(&format!("  change   pm worker modify --name {profile} …\n"));
    out.push_str(&format!("  apply    pm worker restart --name {profile}\n"));
    if restart == RestartPolicy::No {
        out.push_str(
            "\nnothing will restart this worker: --restart no leaves the runtime \n\
             supervising nothing, so a crash or a reboot ends it.\n",
        );
    }
    out
}

/// What starting an existing container prints. It comes back on the
/// settings it was built with, which only matters when the profile has
/// changed since, so that is the only thing worth saying.
fn start_report(runtime: &str, name: &str, profile: &str, stale: bool) -> String {
    let mut out = format!("{name} started in {runtime}.\n");
    if stale {
        out.push_str(&format!(
            "\nit is running the settings it was built with, and worker {profile} has \n\
             changed since. Rebuild it on the current settings with:\n  \
             pm worker restart --name {profile}\n"
        ));
    }
    out.push_str(&format!("\n  logs     {runtime} logs -f {name}\n"));
    out
}

/// Why a running container must not be replaced, or None when it may
/// be. An indeterminate count refuses like a live one: failing open
/// here would destroy sessions exactly when we cannot see them.
fn replace_refusal(live: Option<usize>, force: bool) -> Option<String> {
    if force {
        return None;
    }
    match live {
        Some(0) => None,
        Some(live) => Some(format!(
            "it has {live} live terminal(s) that replacing would kill"
        )),
        None => Some(
            "its live session count could not be determined, and replacing it may kill live \
             sessions"
                .into(),
        ),
    }
}

fn expected_version_line() -> String {
    format!("pm {}", env!("CARGO_PKG_VERSION"))
}

/// Runs the image's Linux `pm --version` before handing over. A stale or
/// malformed image fails here instead of as an opaque detached-container exit.
fn preflight_version(runtime: RuntimeKind, image: &str) -> anyhow::Result<()> {
    let out = runtime_output(runtime, &["run", "--rm", image, CONTAINER_BIN, "--version"])?;
    if !out.status.success() {
        bail!(
            "the pm binary built into image {image} failed to run: {}\n\
             rebuild the image with `{IMAGE_BUILD_HINT}`",
            String::from_utf8_lossy(&out.stderr).trim()
        );
    }
    verify_reported_version(String::from_utf8_lossy(&out.stdout).trim())
}

fn verify_reported_version(reported: &str) -> anyhow::Result<()> {
    let expected = expected_version_line();
    if reported != expected {
        bail!(
            "version drift: the container reports {reported:?} but this launcher is {expected:?}; \
             refusing to hand over"
        );
    }
    Ok(())
}

/// What the launcher needs from a container runtime. Each runtime owns
/// the shape of its own invocation; everything above this line — mount
/// parsing, naming, listener validation — is shared.
trait SandboxRuntime {
    /// Image used when `--image` is omitted.
    fn default_image(&self) -> &'static str;
    /// What the worker inside reports as its runtime, which is also the
    /// command an operator manages the container with.
    fn runtime_name(&self) -> &'static str;
    /// The container name this runtime will accept for `base`.
    fn instance_name(&self, base: &str) -> String {
        base.to_string()
    }
    /// Rejects a `--network` value this runtime cannot express.
    fn validate_network(&self, network: Option<&str>) -> anyhow::Result<()> {
        let _ = network;
        Ok(())
    }
    /// Rejects a limit this runtime cannot apply.
    fn validate_limits(&self, limits: &SandboxLimits) -> anyhow::Result<()> {
        let _ = limits;
        Ok(())
    }
    /// Fails unless the image can be launched.
    fn image_ready(&self, image: &str) -> anyhow::Result<()>;
    /// Version check the runtime can make before committing to a launch.
    fn preflight_version(&self, image: &str) -> anyhow::Result<()>;
    /// Running state of the named container, or None when it does not exist.
    fn container_running(&self, name: &str) -> Option<bool>;
    /// Starts an existing stopped container.
    fn start(&self, name: &str) -> anyhow::Result<()>;
    fn prepare_home(&self, _name: &str, _shifted: bool) -> anyhow::Result<()> {
        Ok(())
    }
    fn stop(&self, name: &str) -> anyhow::Result<()>;
    /// Streams the container's log to this terminal.
    fn logs(&self, name: &str, follow: bool) -> anyhow::Result<()>;
    fn home_pool(&self, _name: &str) -> anyhow::Result<Option<String>> {
        Ok(None)
    }
    fn remove_home_volume(&self, name: &str, pool: Option<&str>) -> anyhow::Result<bool>;
    /// The settings digest recorded when the container was built, or
    /// None from a container built before the label existed.
    fn spec_fingerprint(&self, name: &str) -> Option<String>;
    fn live_terminal_count(&self, name: &str) -> Option<usize>;
    fn remove(&self, name: &str) -> anyhow::Result<()>;
    fn launch(&self, spec: &LaunchSpec) -> anyhow::Result<()>;
}

/// Docker and Podman, which share an invocation surface.
struct OciRuntime {
    kind: RuntimeKind,
}

impl SandboxRuntime for OciRuntime {
    fn default_image(&self) -> &'static str {
        DEFAULT_IMAGE
    }

    fn runtime_name(&self) -> &'static str {
        self.kind.command()
    }

    fn image_ready(&self, image: &str) -> anyhow::Result<()> {
        if image_exists(self.kind, image) {
            return Ok(());
        }
        match missing_image_action(image) {
            MissingImage::Pull => pull_image(self.kind, image),
            MissingImage::Build => {
                bail!("image {image} not found; build it with `{IMAGE_BUILD_HINT}`")
            }
            MissingImage::Unknown => bail!("image {image} not found"),
        }
    }

    fn validate_limits(&self, limits: &SandboxLimits) -> anyhow::Result<()> {
        let unsupported = limits.incus_only();
        if !unsupported.is_empty() {
            bail!(
                "{} has no equivalent on {}: drop it, or pass --runtime incus",
                unsupported.join(" and "),
                self.kind.command()
            );
        }
        if limits.oci_swap_needs_memory() {
            bail!(
                "--memory-swap false needs --memory on {}: it caps memory plus swap as one \
                 total, so denying swap means naming the memory cap",
                self.kind.command()
            );
        }
        Ok(())
    }

    fn preflight_version(&self, image: &str) -> anyhow::Result<()> {
        preflight_version(self.kind, image)
    }

    fn container_running(&self, name: &str) -> Option<bool> {
        container_running(self.kind, name)
    }

    fn live_terminal_count(&self, name: &str) -> Option<usize> {
        live_terminal_count(self.kind, name)
    }

    fn start(&self, name: &str) -> anyhow::Result<()> {
        let out = runtime_output(self.kind, &["start", name])?;
        if !out.status.success() {
            bail!(
                "starting existing container {name} failed: {}",
                String::from_utf8_lossy(&out.stderr).trim()
            );
        }
        Ok(())
    }

    fn stop(&self, name: &str) -> anyhow::Result<()> {
        let out = runtime_output(self.kind, &["stop", name])?;
        if !out.status.success() {
            bail!(
                "stopping container {name} failed: {}",
                String::from_utf8_lossy(&out.stderr).trim()
            );
        }
        Ok(())
    }

    fn logs(&self, name: &str, follow: bool) -> anyhow::Result<()> {
        let command = self.kind.command();
        let mut args = vec!["logs"];
        if follow {
            args.push("--follow");
        }
        args.push(name);
        let status = Command::new(command)
            .args(&args)
            .status()
            .with_context(|| format!("running {command} logs"))?;
        if !status.success() {
            bail!("{command} logs {name} exited with {status}");
        }
        Ok(())
    }

    fn remove_home_volume(&self, name: &str, _pool: Option<&str>) -> anyhow::Result<bool> {
        let volume = home_volume_name(name);
        let listed = runtime_output(self.kind, &["volume", "ls", "--format", "{{.Name}}"])?;
        if !listed.status.success() {
            bail!(
                "listing volumes failed: {}",
                String::from_utf8_lossy(&listed.stderr).trim()
            );
        }
        if !volume_list_contains(&String::from_utf8_lossy(&listed.stdout), &volume) {
            return Ok(false);
        }
        let out = runtime_output(self.kind, &["volume", "rm", &volume])?;
        if !out.status.success() {
            bail!(
                "removing volume {volume} failed: {}",
                String::from_utf8_lossy(&out.stderr).trim()
            );
        }
        Ok(true)
    }

    fn spec_fingerprint(&self, name: &str) -> Option<String> {
        let out = runtime_output(
            self.kind,
            &[
                "inspect",
                "--format",
                &format!("{{{{index .Config.Labels \"{SPEC_LABEL}\"}}}}"),
                name,
            ],
        )
        .ok()?;
        if !out.status.success() {
            return None;
        }
        let value = String::from_utf8_lossy(&out.stdout).trim().to_string();
        (!value.is_empty()).then_some(value)
    }

    fn remove(&self, name: &str) -> anyhow::Result<()> {
        let out = runtime_output(self.kind, &["rm", "--force", name])?;
        if !out.status.success() {
            bail!(
                "removing existing container {name} failed: {}",
                String::from_utf8_lossy(&out.stderr).trim()
            );
        }
        Ok(())
    }

    fn launch(&self, spec: &LaunchSpec) -> anyhow::Result<()> {
        let run_args = build_run_args(spec);
        let command = self.kind.command();
        if !spec.foreground {
            let out = Command::new(command)
                .args(&run_args)
                .output()
                .with_context(|| format!("running {command} run"))?;
            if !out.status.success() {
                bail!(
                    "{command} run failed: {}",
                    String::from_utf8_lossy(&out.stderr).trim()
                );
            }
            print!(
                "{}",
                launch_report(command, spec.name, spec.profile, spec.restart)
            );
            Ok(())
        } else {
            let status = Command::new(command)
                .args(&run_args)
                .status()
                .with_context(|| format!("running {command} run"))?;
            if !status.success() {
                bail!("{command} run exited with {status}");
            }
            Ok(())
        }
    }
}

/// Host serving releases, which the container has to resolve before
/// provisioning can fetch anything.
fn release_host(base_url: &str) -> Option<String> {
    let rest = base_url
        .split_once("://")
        .map(|(_, rest)| rest)
        .unwrap_or(base_url);
    let authority = rest.split('/').next()?;
    let host = match authority.strip_prefix('[') {
        Some(bracketed) => bracketed.split_once(']')?.0,
        None => authority
            .rsplit_once(':')
            .map(|(host, _)| host)
            .unwrap_or(authority),
    };
    (!host.is_empty()).then(|| host.to_string())
}

/// Where the container fetches pm from. The installer reads the same
/// variable, so an operator can point both at one mirror.
fn release_base_url() -> String {
    std::env::var("PM_BASE_URL").unwrap_or_else(|_| pm_daemon::update::RELEASE_BASE_URL.to_string())
}

fn shell_quote(value: &str) -> String {
    format!("'{}'", value.replace('\'', r"'\''"))
}

fn systemd_quote(value: &str) -> String {
    format!("\"{}\"", value.replace('\\', r"\\").replace('"', "\\\""))
}

/// The instance idmap that puts the launching user at the worker's uid,
/// or None when no mapping is needed or the host does not permit one.
///
/// A source on a native filesystem keeps the writer's uid, so without this
/// the worker's files come back owned by uid 1000 rather than by whoever
/// launched. A virtiofs source forces its own ownership and ignores this.
fn instance_idmap() -> Option<String> {
    let (uid, gid) = (unsafe { libc::getuid() }, unsafe { libc::getgid() });
    if uid == WORKER_UID && gid == WORKER_UID {
        return None;
    }
    let missing: Vec<String> = [("/etc/subuid", uid), ("/etc/subgid", gid)]
        .iter()
        .filter(|(file, id)| !subid_permits_root(file, *id))
        .map(|(file, id)| format!("  echo 'root:{id}:1' | sudo tee -a {file}"))
        .collect();
    if !missing.is_empty() {
        eprintln!(
            "warning: cannot map uid {uid} onto the sandbox worker: root is not allowed to \
             use it. Files the worker writes to a --dir on a native filesystem will be owned \
             by uid {WORKER_UID} rather than by you. Allow it with:\n{}",
            missing.join("\n")
        );
        return None;
    }
    Some(format!("uid {uid} {WORKER_UID}\ngid {gid} {WORKER_UID}"))
}

/// Whether root may map this host id, per a subuid/subgid file.
fn subid_permits_root(path: &str, id: u32) -> bool {
    let Ok(text) = std::fs::read_to_string(path) else {
        return false;
    };
    let mut ranges = subid_ranges(&text);
    ranges.any(|(start, count)| id >= start && id - start < count)
}

/// The ranges a subuid/subgid file grants to root.
fn subid_ranges(text: &str) -> impl Iterator<Item = (u32, u32)> + '_ {
    text.lines().filter_map(|line| {
        let mut parts = line.trim().split(':');
        (parts.next()? == "root")
            .then(|| Some((parts.next()?.parse().ok()?, parts.next()?.parse().ok()?)))
            .flatten()
    })
}

pub(crate) fn validate_incus_shifted_home(
    runtime: Option<RuntimeKind>,
    shifted: bool,
) -> anyhow::Result<()> {
    if shifted && runtime.is_some_and(|kind| kind != RuntimeKind::Incus) {
        bail!("--incus-shifted-home requires --runtime incus");
    }
    Ok(())
}

fn incus_home_volume_args(verb: &str, pool: &str, volume: &str, shifted: bool) -> Vec<String> {
    vec![
        "storage".into(),
        "volume".into(),
        verb.into(),
        pool.into(),
        volume.into(),
        format!("security.shifted={shifted}"),
    ]
}

fn incus_create_args(spec: &LaunchSpec) -> Vec<String> {
    let mut args: Vec<String> = vec!["create".into(), spec.image.into(), spec.name.into()];
    let mut config = |key: String| {
        args.push("-c".into());
        args.push(key);
    };
    // Nesting is what lets the worker run a container runtime of its own;
    // the two syscall interceptions are what that runtime needs to unpack
    // and write image layers.
    config("security.nesting=true".into());
    config("security.syscalls.intercept.mknod=true".into());
    config("security.syscalls.intercept.setxattr=true".into());
    if let Some(autostart) = spec.restart.as_autostart() {
        config(format!("boot.autostart={autostart}"));
    }
    for limit in spec.limits.incus_config() {
        config(limit);
    }
    if let Some(idmap) = spec.idmap.as_deref() {
        config(format!("raw.idmap={idmap}"));
    }
    // environment.* reaches `incus exec` only, so the unit repeats these.
    config(format!("user.{SPEC_LABEL}={}", spec.spec));
    config(format!("environment.{INNER_MARKER}=1"));
    config(format!(
        "environment.{RUNTIME_MARKER}={}",
        spec.runtime_name
    ));
    config(format!("environment.{CONTAINER_MARKER}={}", spec.name));
    for (key, value) in resolved_env(spec.env) {
        config(format!("environment.{key}={value}"));
    }
    args
}

fn incus_device_args(spec: &LaunchSpec, pool: &str) -> Vec<Vec<String>> {
    let device = |verb: &str, name: &str, rest: Vec<String>| -> Vec<String> {
        let mut args: Vec<String> = vec![
            "config".into(),
            "device".into(),
            verb.into(),
            spec.name.into(),
            name.into(),
        ];
        args.extend(rest);
        args
    };
    let mut devices = vec![device(
        "add",
        HOME_DEVICE,
        vec![
            "disk".into(),
            format!("pool={pool}"),
            format!("source={}", home_volume_name(spec.name)),
            format!("path={CONTAINER_HOME}"),
        ],
    )];
    for (index, mount) in spec.mounts.iter().enumerate() {
        // No shift=true: a virtiofs source cannot carry an idmapped
        // mount and refuses to start, and it forces ownership to the
        // launching user anyway. Native sources are handled by the
        // instance's raw.idmap instead.
        let mut disk = vec![
            "disk".to_string(),
            format!("source={}", mount.host.display()),
            format!("path={}", mount.container.display()),
        ];
        if mount.read_only {
            disk.push("readonly=true".into());
        }
        devices.push(device("add", &format!("dir{index}"), disk));
    }
    if let Some(listen) = spec.listen {
        devices.push(device(
            "add",
            LISTEN_DEVICE,
            vec![
                "proxy".into(),
                format!("listen=tcp:{listen}"),
                format!("connect=tcp:127.0.0.1:{}", listen.port()),
            ],
        ));
    }
    if let Some(network) = spec.network {
        // The NIC comes from the default profile, so it is overridden
        // rather than added.
        devices.push(device(
            "override",
            NIC_DEVICE,
            vec![format!("network={network}")],
        ));
    }
    if let Some(size) = &spec.limits.disk {
        // The root disk comes from the default profile too.
        devices.push(device(
            "override",
            ROOT_DEVICE,
            vec![format!("size={size}")],
        ));
    }
    devices
}

/// Brings a fresh instance up to what the OCI image ships with: the
/// agent CLIs, a uid 1000 worker owning CONTAINER_HOME, and a released
/// pm at CONTAINER_BIN.
fn provision_script(spec: &LaunchSpec, base_url: &str, version: &str) -> String {
    let mut script = String::from("set -eu\nexport DEBIAN_FRONTEND=noninteractive\n");
    if let Some(host) = release_host(base_url) {
        script.push_str(&format!(
            "systemctl is-system-running --wait >/dev/null 2>&1 || true\n\
             waited=0\n\
             while [ \"$waited\" -lt {RESOLVER_WAIT_SECS} ]; do\n\
             \x20   getent hosts {} >/dev/null 2>&1 && break\n\
             \x20   waited=$((waited + 1))\n\
             \x20   sleep 1\n\
             done\n",
            shell_quote(&host)
        ));
    }
    if let Some((host, ip)) = spec.host_entry {
        script.push_str(&format!(
            "printf '%s %s\\n' {} {} >> /etc/hosts\n",
            shell_quote(&ip.to_string()),
            shell_quote(host)
        ));
    }
    script.push_str("apt-get update\n");
    script.push_str(&format!(
        "apt-get install -y --no-install-recommends {}\n",
        PROVISION_PACKAGES.join(" ")
    ));
    script.push_str(&format!(
        "npm install -g {}\n",
        PROVISION_NPM_PACKAGES.join(" ")
    ));
    // The image's own uid 1000 has to go before the worker can take it.
    script.push_str("if id ubuntu >/dev/null 2>&1; then userdel -r ubuntu >/dev/null 2>&1 || userdel ubuntu; fi\n");
    script.push_str(&format!(
        "id -u {WORKER_USER} >/dev/null 2>&1 || \
         useradd -u {WORKER_UID} -s /bin/bash -d {CONTAINER_HOME} -M {WORKER_USER}\n"
    ));
    // Only the mount point is chowned: the volume carries a previous
    // container's enrollment state, whose ownership is already right.
    script.push_str(&format!("mkdir -p {CONTAINER_HOME}\n"));
    script.push_str(&format!(
        "chown {WORKER_USER}:{WORKER_USER} {CONTAINER_HOME}\n"
    ));
    let base_url = base_url.trim_end_matches('/');
    script.push_str(&format!(
        "curl -fsSL {} | PM_BASE_URL={} sh -s -- \
         --version {} --dir {CONTAINER_BIN_DIR}/bin --no-modify-path\n",
        shell_quote(&format!("{base_url}/install.sh")),
        shell_quote(base_url),
        shell_quote(version)
    ));
    // The installer runs as root, so what it leaves behind is root-owned
    // while the unit runs the worker as WORKER_USER. Following the
    // controller's build means writing a new binary beside the running one
    // and renaming over it, which needs the directory as well as the file,
    // so the whole subtree changes hands after the install rather than
    // before it.
    script.push_str(&format!(
        "chown -R {WORKER_USER}:{WORKER_USER} {CONTAINER_BIN_DIR}\n"
    ));
    script.push_str(&install_worker_sudoers_script());
    script
}

/// Grants WORKER_USER passwordless root inside the container. The
/// container is the boundary, so the unit keeps running as WORKER_USER —
/// running it as root would move HOME off the persistent `/home/worker`
/// volume that holds enrollment state, npm caches and agent config.
///
/// The file is checked before it becomes one sudo reads: a malformed
/// drop-in makes sudo refuse to run at all, and nothing watches
/// provisioning, so an unchecked write would leave a container that has
/// lost sudo entirely rather than a provisioning failure.
fn install_worker_sudoers_script() -> String {
    format!(
        "cat > {WORKER_SUDOERS_STAGE} <<'PM_SUDOERS_EOF'\n\
         {}\n\
         PM_SUDOERS_EOF\n\
         visudo -cf {WORKER_SUDOERS_STAGE}\n\
         install -m 0440 -o root -g root {WORKER_SUDOERS_STAGE} {WORKER_SUDOERS_FILE}\n\
         rm -f {WORKER_SUDOERS_STAGE}\n",
        worker_sudoers_rule()
    )
}

fn worker_sudoers_rule() -> String {
    format!("{WORKER_USER} ALL=(ALL) NOPASSWD:ALL")
}

/// The unit that starts the enrolled worker inside the container.
///
/// It pins no `PATH`. The provisioner installs the agents with a
/// system-wide `npm install -g`, which lands them in `/usr/local/bin`,
/// and that is already on a systemd manager's default `PATH`. A worker
/// installed on a host needs the opposite treatment, because its agents
/// usually sit under the operator's home.
fn worker_unit(spec: &LaunchSpec) -> String {
    let mut unit = String::from(
        "[Unit]\n\
         Description=Puppet Master sandboxed worker\n\
         Wants=network-online.target\n\
         After=network-online.target\n\
         \n\
         [Service]\n\
         Type=simple\n",
    );
    unit.push_str(&format!("User={WORKER_USER}\n"));
    unit.push_str(&format!("WorkingDirectory={CONTAINER_HOME}\n"));
    unit.push_str(&format!(
        "Environment={}\n",
        systemd_quote(&format!("HOME={CONTAINER_HOME}"))
    ));
    unit.push_str(&format!(
        "Environment={}\n",
        systemd_quote(&format!("{INNER_MARKER}=1"))
    ));
    unit.push_str(&format!(
        "Environment={}\n",
        systemd_quote(&format!("{RUNTIME_MARKER}={}", spec.runtime_name))
    ));
    unit.push_str(&format!(
        "Environment={}\n",
        systemd_quote(&format!("{CONTAINER_MARKER}={}", spec.name))
    ));
    for (key, value) in resolved_env(spec.env) {
        unit.push_str(&format!(
            "Environment={}\n",
            systemd_quote(&format!("{key}={value}"))
        ));
    }
    let command: Vec<String> = [CONTAINER_BIN.to_string(), "worker".to_string()]
        .into_iter()
        .map(|arg| systemd_quote(&arg))
        .collect();
    unit.push_str(&format!("ExecStart={}\n", command.join(" ")));
    unit.push_str(&format!("Restart={}\n", spec.restart.as_systemd_restart()));
    unit.push_str("RestartSec=2\n\n[Install]\nWantedBy=multi-user.target\n");
    unit
}

fn incus_enrollment_args(spec: &LaunchSpec) -> Vec<String> {
    let mut args = vec![
        "exec".into(),
        spec.name.into(),
        "--".into(),
        "runuser".into(),
        "-u".into(),
        WORKER_USER.into(),
        "--".into(),
        "env".into(),
        format!("HOME={CONTAINER_HOME}"),
        format!("{INNER_MARKER}=1"),
        format!("{RUNTIME_MARKER}={}", spec.runtime_name),
        format!("{CONTAINER_MARKER}={}", spec.name),
        CONTAINER_BIN.into(),
    ];
    args.extend(worker_command_args(spec));
    args.push("--enroll-only".into());
    args
}

fn install_unit_script(unit: &str) -> String {
    format!(
        "set -eu\numask 077\ncat > /etc/systemd/system/{WORKER_UNIT} <<'PM_WORKER_UNIT_EOF'\n\
         {unit}PM_WORKER_UNIT_EOF\n\
         chmod 600 /etc/systemd/system/{WORKER_UNIT}\n\
         systemctl daemon-reload\n\
         systemctl enable {WORKER_UNIT}\n\
         systemctl restart {WORKER_UNIT}\n"
    )
}

fn incus_status_is_running(info: &str) -> bool {
    info.lines()
        .find_map(|line| line.strip_prefix("Status:"))
        .map(|status| status.trim().eq_ignore_ascii_case("running"))
        .unwrap_or(false)
}

/// A system container running systemd, which is what makes a container
/// runtime work inside the sandbox.
struct IncusRuntime;

fn incus_home_volume_error(error: anyhow::Error) -> anyhow::Error {
    if error.to_string().contains("Error: Storage pool not found") {
        anyhow::anyhow!("{error}\nhelp: if Incus was just installed, run `incus admin init`.")
    } else {
        error
    }
}

impl IncusRuntime {
    fn output(&self, args: &[String]) -> anyhow::Result<std::process::Output> {
        Command::new(RuntimeKind::Incus.command())
            .args(args)
            .output()
            .with_context(|| format!("running incus {}", args.join(" ")))
    }

    fn run(&self, args: &[String], what: &str) -> anyhow::Result<()> {
        let out = self.output(args)?;
        if !out.status.success() {
            bail!(
                "{what} failed: {}",
                String::from_utf8_lossy(&out.stderr).trim()
            );
        }
        Ok(())
    }

    /// Pool the default profile puts instance roots in, which is where
    /// the home volume belongs too.
    fn storage_pool(&self) -> String {
        self.output(&[
            "profile".into(),
            "device".into(),
            "get".into(),
            "default".into(),
            "root".into(),
            "pool".into(),
        ])
        .ok()
        .filter(|out| out.status.success())
        .map(|out| String::from_utf8_lossy(&out.stdout).trim().to_string())
        .filter(|pool| !pool.is_empty())
        .unwrap_or_else(|| INCUS_FALLBACK_POOL.to_string())
    }

    /// The volume outlives the instance, so a replaced container keeps
    /// the worker's enrollment.
    fn ensure_home_volume(&self, pool: &str, volume: &str, shifted: bool) -> anyhow::Result<()> {
        let exists = self
            .output(&[
                "storage".into(),
                "volume".into(),
                "show".into(),
                pool.into(),
                volume.into(),
            ])
            .map(|out| out.status.success())
            .unwrap_or(false);
        if exists {
            return self.run(
                &incus_home_volume_args("set", pool, volume, shifted),
                &format!("configuring storage volume {volume}"),
            );
        }
        self.run(
            &incus_home_volume_args("create", pool, volume, shifted),
            &format!("creating storage volume {volume}"),
        )
        .map_err(incus_home_volume_error)
    }

    /// Feeds a script to a shell inside the container. Output is held
    /// back until something fails, so a routine provision stays quiet
    /// and a failed one reports what the container printed.
    fn exec_script(&self, name: &str, script: &str, what: &str) -> anyhow::Result<()> {
        use std::io::Write as _;
        use std::process::Stdio;

        let mut child = Command::new(RuntimeKind::Incus.command())
            .args(["exec", name, "--", "sh", "-s"])
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn()
            .with_context(|| format!("running incus exec {name}"))?;
        child
            .stdin
            .take()
            .expect("piped stdin")
            .write_all(script.as_bytes())
            .with_context(|| format!("{what}: writing the script to {name}"))?;
        let out = child
            .wait_with_output()
            .with_context(|| format!("running incus exec {name}"))?;
        if !out.status.success() {
            bail!(
                "{what} failed:\n{}\n{}",
                String::from_utf8_lossy(&out.stdout).trim(),
                String::from_utf8_lossy(&out.stderr).trim()
            );
        }
        Ok(())
    }

    fn exec_output(&self, name: &str, command: &[&str]) -> anyhow::Result<std::process::Output> {
        let mut args: Vec<String> = vec!["exec".into(), name.into(), "--".into()];
        args.extend(command.iter().map(|c| c.to_string()));
        self.output(&args)
    }

    fn follow_hint(name: &str) -> String {
        format!("incus exec {name} -- journalctl --unit {WORKER_UNIT} --follow")
    }
}

impl SandboxRuntime for IncusRuntime {
    fn default_image(&self) -> &'static str {
        DEFAULT_INCUS_IMAGE
    }

    fn runtime_name(&self) -> &'static str {
        RuntimeKind::Incus.command()
    }

    fn instance_name(&self, base: &str) -> String {
        incus_instance_name(base)
    }

    fn validate_network(&self, network: Option<&str>) -> anyhow::Result<()> {
        if network == Some("host") {
            bail!(
                "--network host has no Incus equivalent: a container always gets its own \
                 network namespace. Pass the name of an Incus network, or --runtime docker."
            );
        }
        Ok(())
    }

    /// Nothing to check: the image is a remote alias, and a bad one fails
    /// loudly when the instance is created.
    fn image_ready(&self, _image: &str) -> anyhow::Result<()> {
        Ok(())
    }

    /// There is no throwaway equivalent of `run --rm` here, and launching
    /// costs a full provision, so the version check runs on the
    /// provisioned instance instead.
    fn preflight_version(&self, _image: &str) -> anyhow::Result<()> {
        Ok(())
    }

    fn container_running(&self, name: &str) -> Option<bool> {
        let out = self.output(&["info".into(), name.into()]).ok()?;
        if !out.status.success() {
            return None;
        }
        Some(incus_status_is_running(&String::from_utf8_lossy(
            &out.stdout,
        )))
    }

    fn start(&self, name: &str) -> anyhow::Result<()> {
        let out = self.output(&["start".into(), name.into()])?;
        if !out.status.success() {
            bail!(
                "starting existing instance {name} failed: {}",
                String::from_utf8_lossy(&out.stderr).trim()
            );
        }
        Ok(())
    }

    fn stop(&self, name: &str) -> anyhow::Result<()> {
        let out = self.output(&["stop".into(), name.into()])?;
        if !out.status.success() {
            bail!(
                "stopping instance {name} failed: {}",
                String::from_utf8_lossy(&out.stderr).trim()
            );
        }
        Ok(())
    }

    /// The worker runs under a unit inside the instance, so its log is
    /// that unit's rather than the instance's console.
    fn logs(&self, name: &str, follow: bool) -> anyhow::Result<()> {
        let mut args: Vec<String> = vec![
            "exec".into(),
            name.into(),
            "--".into(),
            "journalctl".into(),
            "-u".into(),
            WORKER_UNIT.into(),
        ];
        if follow {
            args.push("-f".into());
        }
        let status = Command::new(RuntimeKind::Incus.command())
            .args(&args)
            .status()
            .context("running incus exec journalctl")?;
        if !status.success() {
            bail!("reading {name} journal exited with {status}");
        }
        Ok(())
    }

    fn home_pool(&self, name: &str) -> anyhow::Result<Option<String>> {
        let out = self.output(&[
            "config".into(),
            "device".into(),
            "get".into(),
            name.into(),
            HOME_DEVICE.into(),
            "pool".into(),
        ])?;
        if !out.status.success() {
            bail!(
                "reading the home storage pool for {name}: {}",
                String::from_utf8_lossy(&out.stderr).trim()
            );
        }
        let pool = String::from_utf8_lossy(&out.stdout).trim().to_string();
        if pool.is_empty() {
            bail!("the home device for {name} has no storage pool");
        }
        Ok(Some(pool))
    }

    fn remove_home_volume(&self, name: &str, pool: Option<&str>) -> anyhow::Result<bool> {
        let pool = pool
            .map(str::to_string)
            .unwrap_or_else(|| self.storage_pool());
        let volume = home_volume_name(name);
        let out = self.output(&[
            "storage".into(),
            "volume".into(),
            "list".into(),
            pool.clone(),
            "--format=csv".into(),
            "--columns=n".into(),
        ])?;
        if !out.status.success() {
            bail!(
                "listing volumes in {pool} failed: {}",
                String::from_utf8_lossy(&out.stderr).trim()
            );
        }
        if !volume_list_contains(&String::from_utf8_lossy(&out.stdout), &volume) {
            return Ok(false);
        }
        self.run(
            &[
                "storage".into(),
                "volume".into(),
                "delete".into(),
                pool,
                volume.clone(),
            ],
            &format!("removing storage volume {volume}"),
        )?;
        Ok(true)
    }

    /// Instance config carries it, under the same key the OCI runtimes
    /// put in a label.
    fn spec_fingerprint(&self, name: &str) -> Option<String> {
        let out = self
            .output(&[
                "config".into(),
                "get".into(),
                name.into(),
                format!("user.{SPEC_LABEL}"),
            ])
            .ok()?;
        if !out.status.success() {
            return None;
        }
        let value = String::from_utf8_lossy(&out.stdout).trim().to_string();
        (!value.is_empty()).then_some(value)
    }

    fn live_terminal_count(&self, name: &str) -> Option<usize> {
        let out = self
            .exec_output(name, &["sh", "-c", LIVE_TERMINALS_PROBE])
            .ok()?;
        if !out.status.success() {
            return None;
        }
        Some(count_children(&String::from_utf8_lossy(&out.stdout)))
    }

    /// Deletes the instance but not its home volume, matching what `rm
    /// --force` leaves behind on the OCI runtimes.
    fn remove(&self, name: &str) -> anyhow::Result<()> {
        self.run(
            &["delete".into(), "--force".into(), name.into()],
            &format!("removing existing container {name}"),
        )
    }

    fn prepare_home(&self, name: &str, shifted: bool) -> anyhow::Result<()> {
        let pool = self
            .home_pool(name)?
            .ok_or_else(|| anyhow::anyhow!("the home device for {name} has no storage pool"))?;
        self.ensure_home_volume(&pool, &home_volume_name(name), shifted)
    }

    fn launch(&self, spec: &LaunchSpec) -> anyhow::Result<()> {
        let pool = self.storage_pool();
        self.ensure_home_volume(&pool, &home_volume_name(spec.name), spec.incus_shifted_home)?;
        self.run(&incus_create_args(spec), "creating the container")?;
        for device in incus_device_args(spec, &pool) {
            self.run(&device, "attaching a device")?;
        }
        self.run(
            &["start".into(), spec.name.into()],
            "starting the container",
        )?;

        let base_url = release_base_url();
        eprintln!("provisioning {}, which takes about a minute", spec.name);
        self.exec_script(
            spec.name,
            &provision_script(spec, &base_url, env!("CARGO_PKG_VERSION")),
            "provisioning the container",
        )?;

        let out = self.exec_output(spec.name, &[CONTAINER_BIN, "--version"])?;
        if !out.status.success() {
            bail!(
                "the pm binary installed into {} failed to run: {}",
                spec.name,
                String::from_utf8_lossy(&out.stderr).trim()
            );
        }
        verify_reported_version(String::from_utf8_lossy(&out.stdout).trim())?;

        if spec.token.is_some() {
            self.run(&incus_enrollment_args(spec), "enrolling the worker")?;
        }

        self.exec_script(
            spec.name,
            &install_unit_script(&worker_unit(spec)),
            "installing the worker service",
        )?;

        println!("{}", spec.name);
        println!("follow with: {}", Self::follow_hint(spec.name));
        if !spec.foreground {
            return Ok(());
        }
        // The container is already running under systemd, so the
        // foreground here is the log stream, not the worker itself.
        eprintln!("the container keeps running after you stop following");
        let status = Command::new(RuntimeKind::Incus.command())
            .args([
                "exec",
                spec.name,
                "--",
                "journalctl",
                "--unit",
                WORKER_UNIT,
                "--follow",
            ])
            .status()
            .with_context(|| format!("following {}", spec.name))?;
        if !status.success() {
            bail!("following {} exited with {status}", spec.name);
        }
        Ok(())
    }
}

fn runtime_for(kind: RuntimeKind) -> Box<dyn SandboxRuntime> {
    match kind {
        RuntimeKind::Docker | RuntimeKind::Podman => Box::new(OciRuntime { kind }),
        RuntimeKind::Incus => Box::new(IncusRuntime),
    }
}

/// Why a container will not reach this controller, or None when there
/// is no reason to think it will not.
///
/// Two shapes of the same mistake. A controller on loopback cannot be
/// reached from a container's network namespace at all. And a name
/// mapped to the gateway reaches the host on its bridge address, which
/// a controller bound only to loopback is not listening on either — the
/// case the lookup-based check above cannot see, because the name it
/// would have resolved is exactly the one that did not resolve.
fn unreachable_controller(
    controller_ip: Option<IpAddr>,
    gateway_host: Option<&str>,
    port: u16,
) -> Option<String> {
    if controller_ip.is_some_and(|ip| ip.is_loopback()) {
        return Some(format!(
            "this controller resolves to loopback, which a container's network namespace \
             cannot reach. Start the daemon with --worker-listen 0.0.0.0:{port}"
        ));
    }
    let host = gateway_host?;
    Some(format!(
        "{host} will reach this machine at its container gateway, so a controller listening \
         only on loopback will not answer. Start the daemon with --worker-listen 0.0.0.0:{port}"
    ))
}

/// Whether an unresolvable controller name should be mapped to the
/// container's gateway.
///
/// A name the launcher cannot resolve is usually one the runtime
/// provides inside the container, which is how Docker Desktop's
/// `host.docker.internal` works. On Linux no runtime provides it, so
/// the name resolves on neither side and the worker cannot reach the
/// controller at all. Mapping it to the gateway there fixes that, and
/// leaving the other platforms alone keeps the runtime's own answer.
fn gateway_host_for<'a>(
    host_entry: &Option<(String, IpAddr)>,
    controller: Option<&'a str>,
    kind: RuntimeKind,
    launcher_os: &str,
) -> Option<&'a str> {
    if host_entry.is_some() || launcher_os != "linux" {
        return None;
    }
    if !matches!(kind, RuntimeKind::Docker | RuntimeKind::Podman) {
        return None;
    }
    let host = controller?;
    // An address needs no mapping, and neither does a name that is
    // already this machine.
    if host.parse::<IpAddr>().is_ok() || host == "localhost" {
        return None;
    }
    Some(host)
}

/// Best-effort launcher-side resolution for an injected host entry. A failed
/// lookup is not an invalid controller: runtimes and custom container networks
/// may provide names that only exist inside the container (for example Docker
/// Desktop's `host.docker.internal`).
fn resolve_host_entry(host: &str, port: u16) -> Option<(String, IpAddr)> {
    resolve_host_entry_with(host, port, |host, port| {
        (host, port).to_socket_addrs().map(|addrs| addrs.collect())
    })
}

fn resolve_host_entry_with(
    host: &str,
    port: u16,
    resolve: impl FnOnce(&str, u16) -> std::io::Result<Vec<SocketAddr>>,
) -> Option<(String, IpAddr)> {
    if host.parse::<IpAddr>().is_ok() {
        return None;
    }
    let addrs = resolve(host, port).ok()?;
    let ip = addrs
        .iter()
        .map(|a| a.ip())
        .find(IpAddr::is_ipv4)
        .or_else(|| addrs.first().map(|a| a.ip()))?;
    Some((host.to_string(), ip))
}

/// Best-effort mount-coverage warning against the local daemon's
/// project list; a remote controller's projects are not visible here.
async fn warn_uncovered_projects(socket: &Path, mounts: &[MountSpec]) {
    let target = pm_client::Target::Unix(socket.to_path_buf());
    let snapshot = tokio::time::timeout(SNAPSHOT_TIMEOUT, crate::fetch_snapshot(&target)).await;
    match snapshot {
        Ok(Ok(snapshot)) => {
            let project_paths: Vec<String> =
                snapshot.projects.iter().map(|p| p.path.clone()).collect();
            for path in uncovered_paths(&project_paths, mounts) {
                eprintln!(
                    "warning: no --dir covers project path {path}; a spawn there will fail \
                     to chdir inside the container"
                );
            }
        }
        _ if mounts.is_empty() => {
            eprintln!(
                "warning: no --dir mounts; sessions can only launch into paths that already \
                 exist inside the image"
            );
        }
        _ => {}
    }
}

pub enum ManageOp {
    Stop,
    Start,
    Logs { follow: bool },
}

/// Rebuilds a worker's container from its stored settings, which is
/// how a change to those settings takes effect.
pub async fn relaunch(
    profile: &str,
    stored: crate::worker::SandboxProfile,
    force: bool,
    socket: &Path,
) -> anyhow::Result<()> {
    launch(args_from_profile(profile, stored, force, true), socket).await
}

/// The launch a stored profile describes. Only the enrollment is absent:
/// the container already holds its credential, so a rebuild needs no
/// token and reconnects from the volume that survives it.
pub fn args_from_profile(
    profile: &str,
    stored: crate::worker::SandboxProfile,
    force: bool,
    rebuild: bool,
) -> SandboxArgs {
    // A listening worker has to come back on the address it was
    // enrolled at, so the rebuild reads it back rather than falling
    // through to dialing.
    let listen = crate::worker::stored_listen(profile);
    SandboxArgs {
        profile: profile.to_string(),
        controller: None,
        token: None,
        listen: listen.as_ref().map(|(addr, _, _)| *addr),
        listen_any: listen.as_ref().is_some_and(|(_, any, _)| *any),
        allow_from: listen
            .as_ref()
            .map(|(_, _, from)| from.clone())
            .unwrap_or_default(),
        dirs: stored.dirs,
        restart: stored.restart,
        foreground: false,
        image: stored.image,
        env: stored.env,
        network: stored.network,
        runtime: stored.runtime,
        incus_shifted_home: stored.incus_shifted_home,
        force,
        rebuild,
        limits: stored.limits,
    }
}

fn volume_list_contains(list: &str, volume: &str) -> bool {
    list.lines().any(|name| name.trim() == volume)
}

pub fn delete_resources(
    profile: &str,
    runtime: Option<RuntimeKind>,
) -> anyhow::Result<Vec<String>> {
    let kind = runtime.map_or_else(detect_runtime, Ok)?;
    let handle = runtime_for(kind);
    let name = handle.instance_name(&container_name(profile));
    let mut removed = Vec::new();
    let pool = if handle.container_running(&name).is_some() {
        let pool = handle.home_pool(&name)?;
        handle.remove(&name)?;
        removed.push(format!("container {name}"));
        pool
    } else {
        None
    };
    if handle.remove_home_volume(&name, pool.as_deref())? {
        removed.push(format!("volume {}", home_volume_name(&name)));
    }
    Ok(removed)
}

/// Runs a management operation against a named worker's container.
pub fn manage(profile: &str, runtime: Option<RuntimeKind>, op: ManageOp) -> anyhow::Result<()> {
    let kind = match runtime {
        Some(kind) => kind,
        None => detect_runtime()?,
    };
    let handle = runtime_for(kind);
    let name = handle.instance_name(&container_name(profile));
    if handle.container_running(&name).is_none() {
        bail!(
            "worker {profile} has no container: {kind} knows nothing called {name}. \
             Start it with `pm worker --name {profile}`",
            kind = kind.command()
        );
    }
    match op {
        ManageOp::Stop => {
            handle.stop(&name)?;
            println!("{name} stopped. Start it again with: pm worker --name {profile}");
            Ok(())
        }
        ManageOp::Start => {
            handle.start(&name)?;
            println!("{name} started.");
            Ok(())
        }
        ManageOp::Logs { follow } => handle.logs(&name, follow),
    }
}

pub async fn launch(args: SandboxArgs, socket: &Path) -> anyhow::Result<()> {
    refuse_re_sandbox(std::env::var_os(INNER_MARKER).is_some())?;
    validate_listener_options(args.listen, args.listen_any, &args.allow_from)?;

    let kind = match args.runtime {
        Some(r) => r,
        None => detect_runtime()?,
    };
    validate_incus_shifted_home(Some(kind), args.incus_shifted_home)?;
    let runtime = runtime_for(kind);

    // A listening sandbox has no controller URL: the controller reaches the
    // published host address. Only a dialing sandbox consults stored config.
    let controller = sandbox_controller(args.controller.as_deref(), args.listen, &args.profile)?;
    let (host, port) = match (args.listen, controller.as_deref()) {
        (Some(listen), _) => (listen.ip().to_string(), listen.port()),
        (None, Some(controller)) => controller_endpoint(controller)?,
        (None, None) => unreachable!("dialing sandbox checked its controller"),
    };

    runtime.validate_network(args.network.as_deref())?;
    runtime.validate_limits(&args.limits)?;

    let mounts = args
        .dirs
        .iter()
        .map(|d| parse_mount_spec(d))
        .collect::<anyhow::Result<Vec<_>>>()?;
    for mount in &mounts {
        if !mount.host.is_dir() {
            bail!(
                "--dir host path {} does not exist or is not a directory",
                mount.host.display()
            );
        }
    }
    warn_uncovered_projects(socket, &mounts).await;

    let image = args
        .image
        .clone()
        .unwrap_or_else(|| runtime.default_image().into());
    runtime.image_ready(&image)?;

    let host_entry = controller
        .as_ref()
        .and_then(|_| resolve_host_entry(&host, port));
    let gateway_host = controller.as_ref().and_then(|_| {
        gateway_host_for(&host_entry, Some(host.as_str()), kind, std::env::consts::OS)
    });
    let controller_ip = controller.as_ref().and_then(|_| {
        host_entry
            .as_ref()
            .map(|(_, ip)| *ip)
            .or_else(|| host.parse().ok())
    });
    if args.network.as_deref() != Some("host") {
        if let Some(warning) = unreachable_controller(controller_ip, gateway_host, port) {
            eprintln!("warning: {warning}");
        }
    }

    runtime.preflight_version(&image)?;

    let spec = spec_fingerprint(&args, &image, &mounts);
    let name = runtime.instance_name(&container_name(&args.profile));
    match runtime.container_running(&name) {
        Some(true) => {
            let live = runtime.live_terminal_count(&name);
            if let Some(reason) = replace_refusal(live, args.force) {
                bail!(
                    "refusing to replace running container {name}: {reason} \
                     (pass --force to replace anyway)"
                );
            }
            runtime.remove(&name)?;
        }
        // A container that exists and is merely stopped is started
        // rather than rebuilt: its home volume, its enrollment and its
        // agent state are all still there, and rebuilding would cost a
        // pull and a provision for nothing. It comes back on the
        // settings it was built with, so a profile edited since then is
        // reported rather than silently ignored.
        Some(false) if args.rebuild => {
            runtime.remove(&name)?;
        }
        Some(false) => {
            runtime.prepare_home(&name, args.incus_shifted_home)?;
            runtime.start(&name)?;
            let stale = runtime
                .spec_fingerprint(&name)
                .is_some_and(|recorded| recorded != spec_fingerprint(&args, &image, &mounts));
            println!(
                "{}",
                start_report(runtime.runtime_name(), &name, &args.profile, stale)
            );
            return Ok(());
        }
        None => {}
    }

    runtime.launch(&LaunchSpec {
        name: &name,
        runtime_name: runtime.runtime_name(),
        image: &image,
        mounts: &mounts,
        restart: args.restart,
        foreground: args.foreground,
        spec: &spec,
        profile: &args.profile,
        network: args.network.as_deref(),
        env: &args.env,
        host_entry: host_entry.as_ref().map(|(h, ip)| (h.as_str(), *ip)),
        gateway_host,
        controller: controller.as_deref(),
        token: args.token.as_deref(),
        listen: args.listen,
        allow_from: &args.allow_from,
        limits: &args.limits,
        idmap: (kind == RuntimeKind::Incus).then(instance_idmap).flatten(),
        incus_shifted_home: args.incus_shifted_home,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn volume_lookup_matches_only_the_exact_managed_name() {
        assert!(volume_list_contains(
            "other\npm-worker-build-home\n",
            "pm-worker-build-home"
        ));
        assert!(!volume_list_contains(
            "pm-worker-build-home-backup\n",
            "pm-worker-build-home"
        ));
        assert!(!volume_list_contains("", "pm-worker-build-home"));
    }

    #[test]
    fn bare_dir_mounts_at_identical_path_read_write() {
        let m = parse_mount_spec("/srv/repos").unwrap();
        assert_eq!(
            m,
            MountSpec {
                host: "/srv/repos".into(),
                container: "/srv/repos".into(),
                read_only: false,
            }
        );
    }

    #[test]
    fn dir_with_mode_only_keeps_identical_path() {
        let m = parse_mount_spec("/srv/repos:ro").unwrap();
        assert_eq!(m.host, m.container);
        assert!(m.read_only);
    }

    #[test]
    fn dir_with_container_path_and_mode() {
        let m = parse_mount_spec("/a:/b:ro").unwrap();
        assert_eq!(m.host, Path::new("/a"));
        assert_eq!(m.container, Path::new("/b"));
        assert!(m.read_only);
        let m = parse_mount_spec("/a:/b").unwrap();
        assert!(!m.read_only);
    }

    #[test]
    fn dir_rejects_relative_bad_mode_and_extra_parts() {
        assert!(parse_mount_spec("srv/repos").is_err());
        assert!(parse_mount_spec("/a:b").is_err());
        assert!(parse_mount_spec("/a:/b:rx").is_err());
        assert!(parse_mount_spec("/a:/b:ro:x").is_err());
        assert!(parse_mount_spec("").is_err());
    }

    #[test]
    fn incus_missing_storage_pool_suggests_initialization() {
        let message = "creating storage volume worker-home failed: Error: Storage pool not found";
        let error = incus_home_volume_error(anyhow::anyhow!(message));
        assert_eq!(
            error.to_string(),
            format!("{message}\nhelp: if Incus was just installed, run `incus admin init`.")
        );
    }

    #[test]
    fn incus_other_storage_errors_keep_their_context() {
        let error = anyhow::anyhow!("permission denied")
            .context("creating storage volume worker-home failed");
        let expected = format!("{error:#}");
        assert_eq!(format!("{:#}", incus_home_volume_error(error)), expected);
    }

    #[test]
    fn dir_rejects_shadowing_image_dirs() {
        assert!(parse_mount_spec("/x:/usr").is_err());
        assert!(parse_mount_spec("/x:/etc").is_err());
        assert!(parse_mount_spec("/x:/home/worker").is_err());
        assert!(parse_mount_spec("/x:/").is_err());
        assert!(parse_mount_spec("/x:/opt/pm").is_err());
        assert!(parse_mount_spec("/x:/opt/pm/bin").is_err());
        assert!(parse_mount_spec("/x:/opt").is_err());
        assert!(parse_mount_spec("/x:/usr/local/bin").is_ok());
        assert!(parse_mount_spec("/x:/home/worker/.claude").is_ok());
        assert!(parse_mount_spec("/x:/etcetera").is_ok());
    }

    #[test]
    fn restart_policy_maps_to_runtime_args() {
        assert_eq!(RestartPolicy::No.as_runtime_arg(), "no");
        assert_eq!(RestartPolicy::OnFailure.as_runtime_arg(), "on-failure");
        assert_eq!(RestartPolicy::Always.as_runtime_arg(), "always");
        assert_eq!(
            RestartPolicy::UnlessStopped.as_runtime_arg(),
            "unless-stopped"
        );
    }

    #[test]
    fn controller_endpoint_parses_schemes_ports_and_ipv6() {
        assert_eq!(
            controller_endpoint("wss://host.lima.internal:7676").unwrap(),
            ("host.lima.internal".to_string(), 7676)
        );
        assert_eq!(
            controller_endpoint("http://h/path").unwrap(),
            ("h".to_string(), 80)
        );
        assert_eq!(
            controller_endpoint("https://h").unwrap(),
            ("h".to_string(), 443)
        );
        assert_eq!(
            controller_endpoint("ws://[::1]:7676").unwrap(),
            ("::1".to_string(), 7676)
        );
        assert!(controller_endpoint("host:7676").is_err());
        assert!(controller_endpoint("ws://h:notaport").is_err());
        assert!(controller_endpoint("ws://").is_err());
    }

    #[test]
    fn unresolved_controller_name_is_left_for_container_dns() {
        let entry = resolve_host_entry_with("host.docker.internal", 7676, |_, _| {
            Err(std::io::Error::new(
                std::io::ErrorKind::NotFound,
                "launcher DNS does not know this name",
            ))
        });
        assert_eq!(entry, None);
    }

    #[test]
    fn resolved_controller_name_prefers_ipv4_host_entry() {
        let entry = resolve_host_entry_with("controller.internal", 7676, |_, _| {
            Ok(vec![
                "[2001:db8::1]:7676".parse().unwrap(),
                "192.0.2.10:7676".parse().unwrap(),
            ])
        });
        assert_eq!(
            entry,
            Some((
                "controller.internal".to_string(),
                "192.0.2.10".parse().unwrap()
            ))
        );
    }

    /// A launch used to print the container name and nothing else, and
    /// only when detached, so an operator had no idea what to type next.
    #[test]
    fn a_launch_reports_the_container_and_how_to_reach_it() {
        let report = launch_report(
            "docker",
            "pm-worker-repos",
            "repos",
            RestartPolicy::UnlessStopped,
        );
        assert!(
            report.contains("pm-worker-repos is running in docker."),
            "{report}"
        );
        assert!(
            report.contains("docker logs -f pm-worker-repos"),
            "{report}"
        );
        assert!(report.contains("docker stop pm-worker-repos"), "{report}");
        assert!(report.contains("docker start pm-worker-repos"), "{report}");
        assert!(report.contains("pm worker modify --name repos"), "{report}");
        assert!(
            report.contains("pm worker restart --name repos"),
            "{report}"
        );
    }

    /// `--restart no` leaves the runtime supervising nothing, which is
    /// the one case where the policy is worth saying out loud.
    #[test]
    fn a_launch_with_no_restart_policy_says_nothing_will_bring_it_back() {
        let supervised = launch_report("docker", "c", "p", RestartPolicy::UnlessStopped);
        assert!(!supervised.contains("nothing will restart"), "{supervised}");
        let unsupervised = launch_report("docker", "c", "p", RestartPolicy::No);
        assert!(
            unsupervised.contains("nothing will restart"),
            "{unsupervised}"
        );
    }

    /// Starting an existing container reuses the settings it was built
    /// with, so a profile that has moved on since is worth saying.
    #[test]
    fn starting_a_stale_container_says_the_settings_have_moved() {
        let current = start_report("docker", "pm-worker-repos", "repos", false);
        assert!(current.contains("started in docker"), "{current}");
        assert!(!current.contains("restart --name"), "{current}");
        let stale = start_report("docker", "pm-worker-repos", "repos", true);
        assert!(stale.contains("pm worker restart --name repos"), "{stale}");
    }

    /// The digest exists to answer one question: were these the settings
    /// the container was built from.
    #[test]
    fn the_spec_digest_changes_with_any_setting_that_builds_the_container() {
        let base = SandboxArgs {
            profile: "repos".into(),
            controller: None,
            token: None,
            listen: None,
            listen_any: false,
            allow_from: Vec::new(),
            dirs: Vec::new(),
            restart: RestartPolicy::UnlessStopped,
            foreground: false,
            image: None,
            env: Vec::new(),
            network: None,
            runtime: None,
            incus_shifted_home: false,
            force: false,
            rebuild: false,
            limits: SandboxLimits::default(),
        };
        let mounts = vec![parse_mount_spec("/srv/repos:ro").unwrap()];
        let reference = spec_fingerprint(&base, "img:1", &mounts);
        assert_eq!(
            reference,
            spec_fingerprint(&base, "img:1", &mounts),
            "the same settings digest the same"
        );
        assert_ne!(reference, spec_fingerprint(&base, "img:2", &mounts));
        assert_ne!(reference, spec_fingerprint(&base, "img:1", &[]));
        let mut shifted = base.clone();
        shifted.incus_shifted_home = true;
        assert_ne!(reference, spec_fingerprint(&shifted, "img:1", &mounts));
        let mut changed = base.clone();
        changed.restart = RestartPolicy::Always;
        assert_ne!(reference, spec_fingerprint(&changed, "img:1", &mounts));
        let mut with_env = base.clone();
        with_env.env = vec!["A=1".into()];
        assert_ne!(reference, spec_fingerprint(&with_env, "img:1", &mounts));
        let mut capped = base.clone();
        capped.limits.memory = Some("8GiB".into());
        assert_ne!(reference, spec_fingerprint(&capped, "img:1", &mounts));
        // Things that do not build the container must not change it, or
        // every ordinary launch would report its own container as stale.
        let mut attached = base.clone();
        attached.foreground = true;
        attached.force = true;
        attached.token = Some("tok".into());
        assert_eq!(reference, spec_fingerprint(&attached, "img:1", &mounts));
    }

    /// The gateway case is the one the resolved-address check cannot
    /// see: the name it would have looked at is the one that failed to
    /// resolve, so without this a worker that silently never registers
    /// is the only symptom.
    #[test]
    fn a_gateway_mapped_name_warns_about_a_loopback_only_controller() {
        let warning =
            unreachable_controller(None, Some("host.docker.internal"), 7677).expect("a warning");
        assert!(warning.contains("container gateway"), "{warning}");
        assert!(
            warning.contains("--worker-listen 0.0.0.0:7677"),
            "{warning}"
        );
    }

    #[test]
    fn a_loopback_controller_still_warns_and_names_the_fix() {
        let warning = unreachable_controller(Some("127.0.0.1".parse().unwrap()), None, 7677)
            .expect("a warning");
        assert!(warning.contains("loopback"), "{warning}");
        assert!(
            warning.contains("--worker-listen 0.0.0.0:7677"),
            "{warning}"
        );
    }

    /// A routable address is the normal case and must stay quiet, or
    /// the warning stops meaning anything.
    #[test]
    fn a_routable_controller_warns_about_nothing() {
        assert_eq!(
            unreachable_controller(Some("10.0.0.2".parse().unwrap()), None, 7677),
            None
        );
        assert_eq!(unreachable_controller(None, None, 7677), None);
    }

    /// `host.docker.internal` is a Docker Desktop name. On Linux the
    /// runtime does not provide it and the launcher cannot resolve it
    /// either, so without this the worker reaches no controller at all.
    #[test]
    fn an_unresolvable_controller_name_maps_to_the_gateway_on_linux() {
        assert_eq!(
            gateway_host_for(
                &None,
                Some("host.docker.internal"),
                RuntimeKind::Docker,
                "linux"
            ),
            Some("host.docker.internal")
        );
        assert_eq!(
            gateway_host_for(
                &None,
                Some("host.containers.internal"),
                RuntimeKind::Podman,
                "linux"
            ),
            Some("host.containers.internal")
        );
    }

    /// Everywhere else the runtime answers the name itself, and
    /// overriding it would replace a working answer with a guess.
    #[test]
    fn a_name_the_runtime_provides_is_left_to_the_runtime() {
        assert_eq!(
            gateway_host_for(
                &None,
                Some("host.docker.internal"),
                RuntimeKind::Docker,
                "macos"
            ),
            None,
            "Docker Desktop resolves it inside the container"
        );
        assert_eq!(
            gateway_host_for(&None, Some("controller"), RuntimeKind::Incus, "linux"),
            None,
            "Incus has no host-gateway, and its provisioner writes /etc/hosts"
        );
    }

    /// A name the launcher resolved needs no gateway, and neither does
    /// something that was never a name.
    #[test]
    fn a_resolved_name_an_address_and_localhost_need_no_gateway() {
        let resolved = Some(("ctl".to_string(), "10.0.0.2".parse().unwrap()));
        assert_eq!(
            gateway_host_for(&resolved, Some("ctl"), RuntimeKind::Docker, "linux"),
            None
        );
        assert_eq!(
            gateway_host_for(&None, Some("10.0.0.2"), RuntimeKind::Docker, "linux"),
            None
        );
        assert_eq!(
            gateway_host_for(&None, Some("localhost"), RuntimeKind::Docker, "linux"),
            None
        );
        assert_eq!(
            gateway_host_for(&None, None, RuntimeKind::Docker, "linux"),
            None
        );
    }

    /// The mapping reaches the runtime as an --add-host, which is what
    /// docker and podman take.
    #[test]
    fn the_gateway_mapping_is_passed_as_an_add_host() {
        let mounts = [];
        let args = build_run_args(&LaunchSpec {
            name: "pm-worker-x",
            runtime_name: "docker",
            image: "img",
            mounts: &mounts,
            restart: RestartPolicy::UnlessStopped,
            foreground: false,
            spec: "s",
            profile: "x",
            network: None,
            env: &[],
            host_entry: None,
            gateway_host: Some("host.docker.internal"),
            controller: Some("wss://host.docker.internal:7677"),
            token: None,
            listen: None,
            allow_from: &[],
            limits: &NO_LIMITS,
            idmap: None,
            incus_shifted_home: false,
        });
        let at = args
            .iter()
            .position(|a| a == "--add-host")
            .expect("an --add-host");
        assert_eq!(args[at + 1], "host.docker.internal:host-gateway");
    }

    /// The name follows the worker's own name and nothing else, which
    /// is what makes it printable and stable: an operator who knows the
    /// worker knows the container, on this launch and the next.
    #[test]
    fn container_name_follows_the_worker_name_alone() {
        assert_eq!(container_name("repos"), "pm-worker-repos");
        assert_eq!(container_name("bench box"), "pm-worker-bench-box");
        assert_eq!(container_name("default"), "pm-worker-default");
        assert_eq!(container_name("repos"), container_name("repos"));
    }

    #[test]
    fn uncovered_paths_reports_only_unmounted_projects() {
        let mounts = vec![
            parse_mount_spec("/srv/repos").unwrap(),
            parse_mount_spec("/data:/mnt/data").unwrap(),
        ];
        let projects = vec![
            String::new(),
            "/srv/repos/api".to_string(),
            "/mnt/data/site".to_string(),
            "/home/user/other".to_string(),
            "/srv/repositories".to_string(),
        ];
        assert_eq!(
            uncovered_paths(&projects, &mounts),
            vec![
                "/home/user/other".to_string(),
                "/srv/repositories".to_string()
            ]
        );
    }

    /// Launching pulls the default image and then refuses it unless its
    /// pm reports the launcher's version, so a tag that is not exactly
    /// that version is a default that can never start.
    #[test]
    fn the_default_image_is_tagged_with_the_version_the_preflight_demands() {
        let (repo, tag) = DEFAULT_IMAGE
            .rsplit_once(':')
            .expect("the default image carries a tag");
        assert_eq!(repo, image_repo!());
        assert_eq!(expected_version_line(), format!("pm {tag}"));
    }

    /// The launcher defaults to an image only the release can produce, so
    /// the two names are read together: a repository renamed in the
    /// script alone leaves every sandbox launch pulling nothing.
    #[test]
    fn the_release_script_pushes_the_repository_the_launcher_defaults_to() {
        const PUBLISH: &str = include_str!("../../../scripts/publish.mjs");
        assert!(
            PUBLISH.contains(concat!("\"", image_repo!(), "\"")),
            "the release script does not push {}",
            image_repo!()
        );
    }

    /// A missing image is three different situations, and answering all
    /// of them with one message is what used to send someone to `make
    /// sandbox-image` for a tag they only had to pull.
    #[test]
    fn a_missing_image_is_pulled_built_or_left_to_the_operator() {
        assert_eq!(missing_image_action(DEFAULT_IMAGE), MissingImage::Pull);
        assert_eq!(missing_image_action(LOCAL_IMAGE), MissingImage::Build);
        assert_eq!(
            missing_image_action(concat!(image_repo!(), ":latest")),
            MissingImage::Unknown,
            "a floating tag is not the version this launcher pairs with"
        );
        assert_eq!(
            missing_image_action("registry.example.com/own:1"),
            MissingImage::Unknown
        );
    }

    #[test]
    fn refuse_re_sandbox_rejects_only_inside_the_marker() {
        assert!(refuse_re_sandbox(true).is_err());
        assert!(refuse_re_sandbox(false).is_ok());
    }

    #[test]
    fn count_children_parses_procfs_list() {
        assert_eq!(count_children(""), 0);
        assert_eq!(count_children("12 45 900 "), 3);
    }

    #[test]
    fn replace_refusal_allows_only_a_known_zero_count() {
        assert_eq!(replace_refusal(Some(0), false), None);
        let live = replace_refusal(Some(3), false).unwrap();
        assert!(
            live.contains("3 live terminal"),
            "unexpected reason: {live}"
        );
    }

    #[test]
    fn replace_refusal_treats_an_indeterminate_count_as_live() {
        let reason = replace_refusal(None, false).unwrap();
        assert!(
            reason.contains("could not be determined"),
            "unexpected reason: {reason}"
        );
    }

    #[test]
    fn replace_refusal_force_overrides_live_and_indeterminate() {
        assert_eq!(replace_refusal(Some(5), true), None);
        assert_eq!(replace_refusal(None, true), None);
        assert_eq!(replace_refusal(Some(0), true), None);
    }

    #[test]
    fn run_args_use_image_binary_and_carry_marker_home_and_entrypoint() {
        let mounts = vec![parse_mount_spec("/srv/repos:ro").unwrap()];
        let args = build_run_args(&LaunchSpec {
            name: "pm-worker-x",
            runtime_name: "docker",
            image: DEFAULT_IMAGE,
            mounts: &mounts,
            restart: RestartPolicy::UnlessStopped,
            foreground: false,
            spec: "testspec",
            profile: "test",
            network: Some("bridge"),
            env: &["A=1".to_string()],
            host_entry: Some(("host.lima.internal", "192.168.5.2".parse().unwrap())),
            gateway_host: None,
            controller: Some("wss://host.lima.internal:7676"),
            token: Some("tok"),
            listen: None,
            allow_from: &[],
            limits: &NO_LIMITS,
            idmap: None,
            incus_shifted_home: false,
        });
        assert_eq!(
            args,
            vec![
                "run",
                "--detach",
                "--label",
                "pm.spec=testspec",
                "--name",
                "pm-worker-x",
                "--restart",
                "unless-stopped",
                "--env",
                "PM_SANDBOX_INNER=1",
                "--env",
                "PM_SANDBOX_RUNTIME=docker",
                "--env",
                "PM_SANDBOX_CONTAINER=pm-worker-x",
                "--add-host",
                "host.lima.internal:192.168.5.2",
                "--network",
                "bridge",
                "--volume",
                "pm-worker-x-home:/home/worker",
                "--volume",
                "/srv/repos:/srv/repos:ro",
                "--env",
                "A=1",
                DEFAULT_IMAGE,
                "/opt/pm/bin/pm",
                "worker",
                "--controller",
                "wss://host.lima.internal:7676",
                "--token",
                "tok",
            ]
        );
    }

    #[test]
    fn run_args_omit_restart_no_and_optional_pieces() {
        let args = build_run_args(&LaunchSpec {
            name: "pm-worker-x",
            runtime_name: "docker",
            image: DEFAULT_IMAGE,
            mounts: &[],
            restart: RestartPolicy::No,
            foreground: true,
            spec: "testspec",
            profile: "test",
            network: None,
            env: &[],
            host_entry: None,
            gateway_host: None,
            controller: Some("ws://192.168.5.2:7676"),
            token: None,
            listen: None,
            allow_from: &[],
            limits: &NO_LIMITS,
            idmap: None,
            incus_shifted_home: false,
        });
        assert!(!args.contains(&"--restart".to_string()));
        assert!(!args.contains(&"--detach".to_string()));
        assert!(!args.contains(&"--add-host".to_string()));
        assert!(!args.contains(&"--network".to_string()));
        assert_eq!(args.last().unwrap(), "ws://192.168.5.2:7676");
    }
}

#[cfg(test)]
mod listen_tests {
    use super::*;

    #[test]
    fn listening_sandbox_needs_no_controller_url() {
        let listen = Some("10.30.2.35:7677".parse().unwrap());
        assert_eq!(sandbox_controller(None, listen, "test").unwrap(), None);
        assert_eq!(
            sandbox_controller(Some("not a controller URL"), listen, "test").unwrap(),
            None,
            "listening mode ignores unrelated saved or supplied controller state"
        );
    }

    #[test]
    fn publishing_every_host_interface_requires_explicit_consent() {
        let listen = Some("0.0.0.0:7677".parse().unwrap());
        let error = validate_listener_options(listen, false, &[])
            .unwrap_err()
            .to_string();
        assert!(error.contains("--listen-any"), "{error}");
        validate_listener_options(listen, true, &["10.30.4.0/24".into()]).unwrap();
        assert!(validate_listener_options(listen, true, &["not-a-cidr".into()]).is_err());
    }

    fn spec_with_listen(
        listen: Option<SocketAddr>,
        network: Option<&str>,
        allow_from: &[String],
    ) -> Vec<String> {
        build_run_args(&LaunchSpec {
            name: "pm-dmz",
            runtime_name: "docker",
            image: LOCAL_IMAGE,
            mounts: &[],
            restart: RestartPolicy::No,
            foreground: true,
            spec: "testspec",
            profile: "test",
            network,
            env: &[],
            host_entry: None,
            gateway_host: None,
            controller: listen.is_none().then_some("wss://controller.internal:7677"),
            token: Some("enrollment"),
            listen,
            allow_from,
            limits: &NO_LIMITS,
            idmap: None,
            incus_shifted_home: false,
        })
    }

    /// A sandboxed host the controller dials has to be reachable from
    /// outside the container, and at the address the operator enrolled.
    #[test]
    fn a_dialed_sandbox_publishes_its_port_and_listens_instead_of_dialing() {
        let allow_from = vec!["10.30.4.0/24".to_string()];
        let args = spec_with_listen(Some("127.0.0.1:7678".parse().unwrap()), None, &allow_from);
        let publish = args
            .iter()
            .position(|a| a == "--publish")
            .expect("--publish");
        assert_eq!(args[publish + 1], "127.0.0.1:7678:7678");
        let listen = args.iter().position(|a| a == "--listen").expect("--listen");
        assert_eq!(
            args[listen + 1],
            "0.0.0.0:7678",
            "the container binds every interface it has; the publish is what narrows it"
        );
        assert!(args.contains(&"--listen-any".to_string()));
        let allow = args
            .iter()
            .position(|a| a == "--allow-from")
            .expect("--allow-from");
        assert_eq!(args[allow + 1], "10.30.4.0/24");
        assert!(
            !args.contains(&"--controller".to_string()),
            "a dialed host has no controller URL to dial"
        );
        assert!(args.contains(&"--token".to_string()));
    }

    #[test]
    fn a_dialing_sandbox_is_unchanged() {
        let args = spec_with_listen(None, None, &[]);
        assert!(!args.contains(&"--publish".to_string()));
        assert!(!args.contains(&"--listen".to_string()));
        let controller = args
            .iter()
            .position(|a| a == "--controller")
            .expect("--controller");
        assert_eq!(args[controller + 1], "wss://controller.internal:7677");
    }

    #[test]
    fn host_networking_binds_the_enrolled_address_without_publishing() {
        let args = spec_with_listen(Some("10.30.2.35:7677".parse().unwrap()), Some("host"), &[]);
        assert!(!args.contains(&"--publish".to_string()));
        let network = args.iter().position(|a| a == "--network").unwrap();
        assert_eq!(args[network + 1], "host");
        let listen = args.iter().position(|a| a == "--listen").unwrap();
        assert_eq!(args[listen + 1], "10.30.2.35:7677");
        assert!(!args.contains(&"--listen-any".to_string()));
    }

    #[test]
    fn ipv6_publish_brackets_the_host_address() {
        let args = spec_with_listen(Some("[::1]:7678".parse().unwrap()), None, &[]);
        let publish = args.iter().position(|a| a == "--publish").unwrap();
        assert_eq!(args[publish + 1], "[::1]:7678:7678");
    }
}

#[cfg(test)]
mod incus_tests {
    use super::*;

    fn spec<'a>(
        mounts: &'a [MountSpec],
        env: &'a [String],
        listen: Option<SocketAddr>,
        network: Option<&'a str>,
        restart: RestartPolicy,
    ) -> LaunchSpec<'a> {
        LaunchSpec {
            name: "pm-worker-x",
            runtime_name: "docker",
            image: DEFAULT_INCUS_IMAGE,
            mounts,
            restart,
            foreground: false,
            spec: "testspec",
            profile: "test",
            network,
            env,
            host_entry: None,
            gateway_host: None,
            controller: listen.is_none().then_some("wss://controller.internal:7676"),
            token: Some("tok"),
            listen,
            allow_from: &[],
            limits: &NO_LIMITS,
            idmap: None,
            incus_shifted_home: false,
        }
    }

    fn flat(devices: &[Vec<String>]) -> Vec<String> {
        devices.iter().map(|d| d.join(" ")).collect()
    }

    #[test]
    fn home_volume_shifting_is_explicit_on_creation_and_reuse() {
        for verb in ["create", "set"] {
            for shifted in [false, true] {
                assert_eq!(
                    incus_home_volume_args(verb, "pool0", "worker-home", shifted),
                    vec![
                        "storage",
                        "volume",
                        verb,
                        "pool0",
                        "worker-home",
                        if shifted {
                            "security.shifted=true"
                        } else {
                            "security.shifted=false"
                        }
                    ]
                );
            }
        }
    }

    #[test]
    fn shifted_home_is_only_supported_by_incus() {
        validate_incus_shifted_home(Some(RuntimeKind::Incus), true).unwrap();
        validate_incus_shifted_home(None, true).unwrap();
        for kind in [RuntimeKind::Docker, RuntimeKind::Podman] {
            assert!(validate_incus_shifted_home(Some(kind), true)
                .unwrap_err()
                .to_string()
                .contains("--runtime incus"));
            validate_incus_shifted_home(Some(kind), false).unwrap();
        }
    }

    #[test]
    fn instance_name_drops_what_incus_rejects_and_leaves_room_for_the_volume() {
        assert_eq!(
            incus_instance_name("pm-worker-host.lima.internal-7676"),
            "pm-worker-host-lima-internal-7676",
            "an instance name is a hostname, so dots cannot survive"
        );
        let long = incus_instance_name(&format!("pm-worker-{}", "a".repeat(120)));
        assert_eq!(long.len(), INCUS_NAME_MAX);
        assert!(
            home_volume_name(&long).len() <= 63,
            "the home volume name has to fit too: {}",
            home_volume_name(&long)
        );
        assert!(
            !incus_instance_name("pm-worker-trailing.").ends_with('-'),
            "a name may not end in a hyphen"
        );
    }

    #[test]
    fn instance_name_is_deterministic() {
        let once = incus_instance_name("pm-worker-host.lima.internal-7676");
        let twice = incus_instance_name("pm-worker-host.lima.internal-7676");
        assert_eq!(once, twice);
    }

    #[test]
    fn restart_policy_maps_onto_both_supervision_layers() {
        for (policy, autostart, unit) in [
            (RestartPolicy::No, Some("false"), "no"),
            (RestartPolicy::OnFailure, Some("false"), "on-failure"),
            (RestartPolicy::Always, Some("true"), "always"),
            (RestartPolicy::UnlessStopped, None, "always"),
        ] {
            assert_eq!(policy.as_autostart(), autostart, "{policy:?}");
            assert_eq!(policy.as_systemd_restart(), unit, "{policy:?}");
        }
    }

    #[test]
    fn subid_ranges_reads_only_root_grants() {
        let text = "builder:524288:65536\nroot:1074266113:1000000000\nroot:501:1\n";
        let ranges: Vec<_> = subid_ranges(text).collect();
        assert_eq!(ranges, vec![(1074266113, 1000000000), (501, 1)]);
    }

    #[test]
    fn a_single_id_grant_covers_only_that_id() {
        let text = "root:501:1\n";
        assert!(subid_ranges(text).any(|(s, c)| 501 >= s && 501 - s < c));
        assert!(!subid_ranges(text).any(|(s, c)| 502 >= s && 502 - s < c));
    }

    fn incus_spec<'a>(mounts: &'a [MountSpec], idmap: Option<String>) -> LaunchSpec<'a> {
        LaunchSpec {
            name: "pm-worker-bm",
            runtime_name: "docker",
            image: "images:ubuntu/26.04",
            mounts,
            restart: RestartPolicy::No,
            foreground: false,
            spec: "testspec",
            profile: "test",
            network: None,
            env: &[],
            host_entry: None,
            gateway_host: None,
            controller: Some("wss://controller.internal:7677"),
            token: Some("enrollment"),
            listen: None,
            allow_from: &[],
            limits: &NO_LIMITS,
            idmap,
            incus_shifted_home: false,
        }
    }

    #[test]
    fn a_dir_mount_does_not_ask_for_shifting() {
        let mounts = vec![parse_mount_spec("/srv/repos").unwrap()];
        let joined = incus_device_args(&incus_spec(&mounts, None), "default")
            .concat()
            .join(" ");
        assert!(
            !joined.contains("shift=true"),
            "a virtiofs source cannot carry an idmapped mount: {joined}"
        );
        assert!(joined.contains("source=/srv/repos"));
    }

    #[test]
    fn an_idmap_reaches_the_instance_config() {
        let idmap = "uid 501 1000\ngid 1000 1000";
        let args = incus_create_args(&incus_spec(&[], Some(idmap.into())));
        assert!(args.contains(&format!("raw.idmap={idmap}")));
        assert!(!incus_create_args(&incus_spec(&[], None))
            .iter()
            .any(|a| a.starts_with("raw.idmap=")));
    }

    #[test]
    fn create_args_carry_nesting_the_marker_and_autostart() {
        let env = vec!["A=1".to_string()];
        let args = incus_create_args(&spec(&[], &env, None, None, RestartPolicy::Always));
        assert_eq!(args[..3], ["create", DEFAULT_INCUS_IMAGE, "pm-worker-x"]);
        for expected in [
            "security.nesting=true",
            "security.syscalls.intercept.mknod=true",
            "security.syscalls.intercept.setxattr=true",
            "boot.autostart=true",
            "environment.PM_SANDBOX_INNER=1",
            "environment.A=1",
        ] {
            assert!(
                args.contains(&expected.to_string()),
                "missing {expected} in {args:?}"
            );
        }
    }

    #[test]
    fn unless_stopped_leaves_autostart_unset_so_incus_restores_last_state() {
        let args = incus_create_args(&spec(&[], &[], None, None, RestartPolicy::UnlessStopped));
        assert!(
            !args.iter().any(|a| a.starts_with("boot.autostart")),
            "{args:?}"
        );
    }

    #[test]
    fn devices_mount_the_home_volume_and_each_dir() {
        let mounts = vec![
            parse_mount_spec("/srv/repos:ro").unwrap(),
            parse_mount_spec("/data:/mnt/data").unwrap(),
        ];
        let devices = flat(&incus_device_args(
            &spec(&mounts, &[], None, None, RestartPolicy::No),
            "pool0",
        ));
        assert_eq!(
            devices[0],
            "config device add pm-worker-x home disk pool=pool0 \
             source=pm-worker-x-home path=/home/worker"
        );
        assert_eq!(
            devices[1],
            "config device add pm-worker-x dir0 disk source=/srv/repos \
             path=/srv/repos readonly=true"
        );
        assert_eq!(
            devices[2],
            "config device add pm-worker-x dir1 disk source=/data \
             path=/mnt/data"
        );
    }

    #[test]
    fn a_dialed_sandbox_gets_a_proxy_device_at_the_enrolled_port() {
        let devices = flat(&incus_device_args(
            &spec(
                &[],
                &[],
                Some("127.0.0.1:7678".parse().unwrap()),
                None,
                RestartPolicy::No,
            ),
            "pool0",
        ));
        assert!(
            devices.contains(
                &"config device add pm-worker-x pm-listen proxy \
                  listen=tcp:127.0.0.1:7678 connect=tcp:127.0.0.1:7678"
                    .to_string()
            ),
            "{devices:?}"
        );
    }

    #[test]
    fn an_ipv6_listener_brackets_the_proxy_address() {
        let devices = flat(&incus_device_args(
            &spec(
                &[],
                &[],
                Some("[::1]:7678".parse().unwrap()),
                None,
                RestartPolicy::No,
            ),
            "pool0",
        ));
        assert!(
            devices
                .iter()
                .any(|d| d.contains("listen=tcp:[::1]:7678 connect=tcp:127.0.0.1:7678")),
            "{devices:?}"
        );
    }

    #[test]
    fn a_named_network_overrides_the_profile_nic() {
        let devices = flat(&incus_device_args(
            &spec(&[], &[], None, Some("incusbr1"), RestartPolicy::No),
            "pool0",
        ));
        assert!(
            devices
                .contains(&"config device override pm-worker-x eth0 network=incusbr1".to_string()),
            "{devices:?}"
        );
    }

    #[test]
    fn host_networking_is_refused_on_incus_and_left_alone_on_docker() {
        let error = IncusRuntime
            .validate_network(Some("host"))
            .unwrap_err()
            .to_string();
        assert!(error.contains("--network host"), "{error}");
        IncusRuntime.validate_network(Some("incusbr0")).unwrap();
        IncusRuntime.validate_network(None).unwrap();
        OciRuntime {
            kind: RuntimeKind::Docker,
        }
        .validate_network(Some("host"))
        .unwrap();
    }

    /// A worker that cannot write the directory its binary lives in can
    /// never follow its controller's build: installing stages a new
    /// binary beside the running one, so the install has to be handed to
    /// the account the unit runs as. The installer runs as root, which
    /// makes chowning after it the only ordering that works.
    #[test]
    fn provisioning_hands_the_install_directory_to_the_worker_user() {
        let script = provision_script(
            &spec(&[], &[], None, None, RestartPolicy::OnFailure),
            "https://releases.example",
            "9.9.9",
        );
        let chown = script
            .find("chown -R worker:worker /opt/pm\n")
            .expect("the install directory is never handed to the worker user");
        let install = script.find("install.sh").expect("installer fetch");
        assert!(
            chown > install,
            "chowning before the root installer runs leaves its files root-owned: {script}"
        );
    }

    /// The packages an `apt-get install` names, whether the list runs on
    /// one line or over shell continuations. Searching the whole text
    /// instead would match the prose around it: `sudo` is a word in a
    /// Dockerfile comment and a substring of `visudo` and `sudoers`.
    fn apt_packages(text: &str) -> Vec<&str> {
        let mut rest = text
            .split_once("--no-install-recommends")
            .expect("an apt-get install invocation")
            .1;
        let mut packages = Vec::new();
        loop {
            let (line, tail) = rest.split_once('\n').unwrap_or((rest, ""));
            let continues = line.trim_end().ends_with('\\');
            let line = line.trim().trim_end_matches('\\');
            if line.starts_with("&&") {
                return packages;
            }
            packages.extend(line.split_whitespace());
            if !continues {
                return packages;
            }
            rest = tail;
        }
    }

    /// The container is the isolation boundary, so the worker holds root
    /// inside it. A drop-in that reaches `/etc/sudoers.d` before `visudo`
    /// has accepted it costs the container sudo altogether, and nothing
    /// watches provisioning run, so the check has to be in the script.
    #[test]
    fn provisioning_grants_the_worker_a_validated_sudoers_drop_in() {
        let script = provision_script(
            &spec(&[], &[], None, None, RestartPolicy::OnFailure),
            "https://releases.example",
            "9.9.9",
        );
        assert!(
            apt_packages(&script).contains(&"sudo"),
            "sudo is inherited from whatever the base image happens to ship: {script}"
        );
        assert!(
            script.contains("worker ALL=(ALL) NOPASSWD:ALL"),
            "the worker is never granted sudo: {script}"
        );
        assert!(
            !WORKER_SUDOERS_STAGE.starts_with("/etc/sudoers.d/"),
            "staging inside /etc/sudoers.d puts an unchecked file where sudo reads it"
        );
        let check = script
            .find(&format!("visudo -cf {WORKER_SUDOERS_STAGE}"))
            .expect("the drop-in is never checked by visudo");
        let install = script
            .find(&format!(
                "install -m 0440 -o root -g root {WORKER_SUDOERS_STAGE} {WORKER_SUDOERS_FILE}"
            ))
            .expect("the drop-in never lands root-owned at 0440");
        assert!(
            check < install,
            "checking after the file is in place is checking too late: {script}"
        );
    }

    /// The provisioner and the image are two hand-maintained copies of
    /// the same setup, and a container built the other way has already
    /// been left behind by a change to only one of them. Both are read
    /// here so neither can move alone.
    #[test]
    fn the_oci_image_grants_the_same_sudo_rule_as_the_provisioner() {
        const DOCKERFILE: &str = include_str!("../../../docker/sandbox-worker/Dockerfile");
        assert!(
            DOCKERFILE.contains(&worker_sudoers_rule()),
            "the image grants a different rule than the provisioner: {DOCKERFILE}"
        );
        assert!(
            DOCKERFILE.contains(&format!("visudo -cf {WORKER_SUDOERS_STAGE}")),
            "the image writes its drop-in unchecked: {DOCKERFILE}"
        );
        assert!(
            DOCKERFILE.contains(&format!(
                "install -m 0440 -o root -g root {WORKER_SUDOERS_STAGE} {WORKER_SUDOERS_FILE}"
            )),
            "the image's drop-in is not root-owned at 0440: {DOCKERFILE}"
        );
        assert_eq!(
            apt_packages(DOCKERFILE),
            PROVISION_PACKAGES,
            "the image and the provisioner install different packages"
        );

        for package in PROVISION_NPM_PACKAGES {
            assert!(
                DOCKERFILE.contains(package),
                "the image is missing {package}, which the provisioner installs: {DOCKERFILE}"
            );
        }
    }

    /// A host worker's unit has to pin PATH because its agents live under
    /// the operator's home, invisible to a systemd manager. The container
    /// is the opposite case and must stay that way: `npm install -g` with
    /// no prefix override puts the agents in /usr/local/bin, which the
    /// default PATH already reaches. A prefix override here would strand
    /// the agents exactly as it did on a host.
    #[test]
    fn the_container_installs_its_agents_where_the_default_path_reaches() {
        const DOCKERFILE: &str = include_str!("../../../docker/sandbox-worker/Dockerfile");
        let script = provision_script(
            &spec(&[], &[], None, None, RestartPolicy::OnFailure),
            "https://example.invalid",
            "0.0.0",
        );

        for (source, text) in [
            ("the provisioner", script.as_str()),
            ("the image", DOCKERFILE),
        ] {
            assert!(
                text.contains(&format!(
                    "npm install -g {}",
                    PROVISION_NPM_PACKAGES.join(" ")
                )),
                "{source} does not install the agents globally"
            );
            for override_form in [
                "npm config set prefix",
                "--prefix",
                "PREFIX=",
                "npm_config_prefix",
            ] {
                assert!(
                    !text.contains(override_form),
                    "{source} moves npm's global prefix with {override_form}, which would put \
                     the agents outside the default PATH the worker unit relies on"
                );
            }
        }

        assert!(
            !worker_unit(&spec(&[], &[], None, None, RestartPolicy::OnFailure)).contains("PATH="),
            "the container unit pins a PATH: if that became necessary, say why here"
        );
    }

    #[test]
    fn the_sandbox_unit_loads_enrollment_from_the_persistent_home() {
        let unit = worker_unit(&spec(&[], &[], None, None, RestartPolicy::OnFailure));
        assert!(
            unit.contains(r#"ExecStart="/opt/pm/bin/pm" "worker""#),
            "{unit}"
        );
        assert!(!unit.contains("--token"), "{unit}");
        assert!(!unit.contains("--controller"), "{unit}");
    }

    #[test]
    fn incus_enrolls_once_as_the_worker_user_before_installing_the_unit() {
        let spec = spec(&[], &[], None, None, RestartPolicy::OnFailure);
        let args = incus_enrollment_args(&spec);
        assert_eq!(
            args,
            vec![
                "exec",
                "pm-worker-x",
                "--",
                "runuser",
                "-u",
                "worker",
                "--",
                "env",
                "HOME=/home/worker",
                "PM_SANDBOX_INNER=1",
                "PM_SANDBOX_RUNTIME=docker",
                "PM_SANDBOX_CONTAINER=pm-worker-x",
                "/opt/pm/bin/pm",
                "worker",
                "--controller",
                "wss://controller.internal:7676",
                "--token",
                "tok",
                "--enroll-only",
            ]
        );
    }

    /// A world-readable unit would hand the token to anything in the
    /// container for as long as the file exists.
    #[test]
    fn the_unit_is_installed_unreadable_to_the_rest_of_the_container() {
        let script = install_unit_script("[Unit]\n");
        assert!(
            script.contains("chmod 600 /etc/systemd/system/pm-worker.service"),
            "{script}"
        );
    }

    #[test]
    fn the_unit_runs_the_worker_as_the_image_user_with_the_marker() {
        let env = vec!["A=1".to_string()];
        let unit = worker_unit(&spec(&[], &env, None, None, RestartPolicy::OnFailure));
        assert!(unit.contains("User=worker"), "{unit}");
        assert!(unit.contains("WorkingDirectory=/home/worker"), "{unit}");
        assert!(
            unit.contains(r#"Environment="HOME=/home/worker""#),
            "{unit}"
        );
        assert!(
            unit.contains(r#"Environment="PM_SANDBOX_INNER=1""#),
            "environment.* only reaches `incus exec`, so the unit has to repeat it: {unit}"
        );
        assert!(unit.contains(r#"Environment="A=1""#), "{unit}");
        assert!(
            unit.contains(r#"ExecStart="/opt/pm/bin/pm" "worker""#),
            "{unit}"
        );
        assert!(!unit.contains("--token"), "{unit}");
        assert!(unit.contains("Restart=on-failure"), "{unit}");
    }

    #[test]
    fn a_listening_incus_worker_enrolls_on_the_published_address_once() {
        let spec = spec(
            &[],
            &[],
            Some("127.0.0.1:7678".parse().unwrap()),
            None,
            RestartPolicy::No,
        );
        let args = incus_enrollment_args(&spec);
        assert!(
            args.windows(3)
                .any(|args| args == ["--listen", "0.0.0.0:7678", "--listen-any"]),
            "the proxy device connects to loopback inside, so the worker binds its own \
             interfaces: {args:?}"
        );
        assert!(!args.contains(&"--controller".to_string()), "{args:?}");
    }

    #[test]
    fn provisioning_gates_on_the_resolver_before_it_fetches_anything() {
        let script = provision_script(
            &spec(&[], &[], None, None, RestartPolicy::No),
            "https://releases.example",
            "9.9.9",
        );
        let gate = script.find("getent hosts").expect("resolver gate");
        let fetch = script.find("install.sh").expect("installer fetch");
        assert!(
            gate < fetch,
            "a fresh instance answers exec before systemd-resolved is up: {script}"
        );
        assert!(script.contains("'releases.example'"), "{script}");
        assert!(script.contains("apt-get update"), "{script}");
    }

    #[test]
    fn provisioning_installs_the_launchers_own_version_into_the_image_path() {
        let script = provision_script(
            &spec(&[], &[], None, None, RestartPolicy::No),
            "https://releases.example/",
            "9.9.9",
        );
        assert!(
            script.contains(
                "curl -fsSL 'https://releases.example/install.sh' | \
                 PM_BASE_URL='https://releases.example' sh -s -- \
                 --version '9.9.9' --dir /opt/pm/bin --no-modify-path"
            ),
            "pinning the version is what keeps the drift check meaningful: {script}"
        );
    }

    #[test]
    fn provisioning_installs_what_the_oci_image_ships() {
        let script = provision_script(
            &spec(&[], &[], None, None, RestartPolicy::No),
            "https://releases.example",
            "9.9.9",
        );
        assert_eq!(apt_packages(&script), PROVISION_PACKAGES, "{script}");
        for package in PROVISION_NPM_PACKAGES {
            assert!(script.contains(package), "missing {package}: {script}");
        }
        assert!(
            script.contains("useradd -u 1000"),
            "the worker uid has to match the image's so enrollment state stays readable: {script}"
        );
        assert!(script.contains("userdel"), "{script}");
    }

    #[test]
    fn an_injected_host_entry_reaches_the_containers_hosts_file() {
        let mut spec = spec(&[], &[], None, None, RestartPolicy::No);
        let ip: IpAddr = "192.168.5.2".parse().unwrap();
        spec.host_entry = Some(("host.lima.internal", ip));
        let script = provision_script(&spec, "https://releases.example", "9.9.9");
        assert!(
            script.contains("printf '%s %s\\n' '192.168.5.2' 'host.lima.internal' >> /etc/hosts"),
            "{script}"
        );
    }

    #[test]
    fn the_unit_installer_writes_and_enables_the_service() {
        let script = install_unit_script("[Unit]\n");
        assert!(
            script.contains("/etc/systemd/system/pm-worker.service"),
            "{script}"
        );
        assert!(script.contains("systemctl daemon-reload"), "{script}");
        assert!(
            script.contains("systemctl enable pm-worker.service"),
            "{script}"
        );
        assert!(
            script.contains("systemctl restart pm-worker.service"),
            "{script}"
        );
    }

    #[test]
    fn running_state_comes_from_the_status_line() {
        assert!(incus_status_is_running(
            "Name: x\nStatus: RUNNING\nType: container\n"
        ));
        assert!(!incus_status_is_running("Name: x\nStatus: STOPPED\n"));
        assert!(
            !incus_status_is_running("Error: Instance not found\n"),
            "an unparseable listing is not a running container"
        );
    }

    #[test]
    fn release_host_drops_the_scheme_port_and_path() {
        assert_eq!(
            release_host("https://releases.example/x"),
            Some("releases.example".into())
        );
        assert_eq!(
            release_host("https://releases.example:8443"),
            Some("releases.example".into())
        );
        assert_eq!(release_host("https://[::1]:8443"), Some("::1".into()));
        assert_eq!(release_host("https://"), None);
    }

    #[test]
    fn quoting_survives_values_that_would_break_out() {
        assert_eq!(shell_quote("it's"), r"'it'\''s'");
        assert_eq!(systemd_quote(r#"a"b\c"#), r#""a\"b\\c""#);
    }
}

#[cfg(test)]
mod limit_tests {
    use super::*;

    fn limits() -> SandboxLimits {
        SandboxLimits {
            cpu: Some("2".into()),
            memory: Some("4GiB".into()),
            memory_swap: Some(false),
            cpu_allowance: Some("50%".into()),
            memory_enforce: Some(MemoryEnforce::Hard),
            disk: Some("40GiB".into()),
        }
    }

    fn docker() -> OciRuntime {
        OciRuntime {
            kind: RuntimeKind::Docker,
        }
    }

    #[test]
    fn unset_limits_cap_nothing_on_either_runtime() {
        let none = SandboxLimits::default();
        assert!(none.incus_config().is_empty());
        assert!(none.oci_args().is_empty());
        docker().validate_limits(&none).unwrap();
        IncusRuntime.validate_limits(&none).unwrap();
    }

    #[test]
    fn every_limit_reaches_incus_as_a_config_key() {
        assert_eq!(
            limits().incus_config(),
            vec![
                "limits.cpu=2",
                "limits.memory=4GiB",
                "limits.memory.swap=false",
                "limits.cpu.allowance=50%",
                "limits.memory.enforce=hard",
            ]
        );
    }

    #[test]
    fn the_disk_cap_sizes_the_root_device_rather_than_a_config_key() {
        let limits = SandboxLimits {
            disk: Some("40GiB".into()),
            ..SandboxLimits::default()
        };
        assert!(limits.incus_config().is_empty());
        let spec = LaunchSpec {
            name: "pm-worker-x",
            runtime_name: "docker",
            image: DEFAULT_INCUS_IMAGE,
            mounts: &[],
            restart: RestartPolicy::No,
            foreground: false,
            spec: "testspec",
            profile: "test",
            network: None,
            env: &[],
            host_entry: None,
            gateway_host: None,
            controller: Some("wss://c:7676"),
            token: None,
            listen: None,
            allow_from: &[],
            limits: &limits,
            idmap: None,
            incus_shifted_home: false,
        };
        let devices: Vec<String> = incus_device_args(&spec, "pool0")
            .iter()
            .map(|d| d.join(" "))
            .collect();
        assert!(
            devices.contains(&"config device override pm-worker-x root size=40GiB".to_string()),
            "{devices:?}"
        );
    }

    #[test]
    fn the_shared_limits_map_onto_docker_and_swap_becomes_a_total() {
        let shared = SandboxLimits {
            cpu: Some("2".into()),
            memory: Some("4GiB".into()),
            memory_swap: Some(false),
            ..SandboxLimits::default()
        };
        assert_eq!(
            shared.oci_args(),
            vec!["--cpus", "2", "--memory", "4GiB", "--memory-swap", "4GiB"],
            "denying swap means capping memory plus swap at the memory cap"
        );
        let swapping = SandboxLimits {
            memory_swap: Some(true),
            ..SandboxLimits::default()
        };
        assert_eq!(swapping.oci_args(), vec!["--memory-swap", "-1"]);
    }

    #[test]
    fn incus_only_limits_are_refused_on_docker_by_name() {
        let error = docker().validate_limits(&limits()).unwrap_err().to_string();
        for flag in ["--cpu-allowance", "--memory-enforce", "--disk"] {
            assert!(error.contains(flag), "{flag} unnamed in: {error}");
        }
        assert!(error.contains("--runtime incus"), "{error}");
        IncusRuntime.validate_limits(&limits()).unwrap();
    }

    #[test]
    fn denying_swap_without_a_memory_cap_is_refused_before_anything_is_replaced() {
        let limits = SandboxLimits {
            memory_swap: Some(false),
            ..SandboxLimits::default()
        };
        let error = docker().validate_limits(&limits).unwrap_err().to_string();
        assert!(error.contains("--memory"), "{error}");
        IncusRuntime
            .validate_limits(&limits)
            .expect("Incus spells swap as its own switch");
    }
}

#[cfg(test)]
mod detection_tests {
    use super::*;

    #[test]
    fn linux_prefers_incus_and_keeps_the_oci_runtimes_behind_it() {
        assert_eq!(
            detection_order(true),
            [RuntimeKind::Incus, RuntimeKind::Docker, RuntimeKind::Podman]
        );
    }

    #[test]
    fn elsewhere_has_no_incus_to_detect() {
        let order = detection_order(false);
        assert_eq!(order, [RuntimeKind::Docker, RuntimeKind::Podman]);
        assert!(
            !order.contains(&RuntimeKind::Incus),
            "Incus has no macOS host support"
        );
    }
}
