//! Generating a systemd unit that keeps `pm worker` running.
//!
//! `pm worker` runs in the foreground, so a worker started by hand dies
//! with the shell that started it. Enrollment completes before installation,
//! and the generated unit loads its connection mode and durable credential
//! from worker.toml.

use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

use anyhow::{bail, Context};

/// Seconds systemd waits before starting a worker that exited.
const RESTART_SEC: u32 = 5;
const UNIT_PREFIX: &str = "pm-worker";
const SYSTEM_UNIT_DIR: &str = "/etc/systemd/system";
/// Where logind puts an account's runtime directory, and the bus socket
/// `systemctl --user` derives its address from.
const RUNTIME_DIR_ROOT: &str = "/run/user";
/// systemd's own fallback `PATH`. A manager started without a login
/// session has nothing richer, which is why an agent installed under the
/// operator's home is invisible to it.
const DEFAULT_PATH: &str = "/usr/local/sbin:/usr/local/bin:/usr/sbin:/usr/bin:/sbin:/bin";
const USER_BUS_SOCKET: &str = "bus";
/// How long to wait for the account's user manager to answer. Enabling
/// lingering starts it asynchronously: the runtime directory appears
/// almost at once but the bus socket lands roughly an eighth of a second
/// later, so a reload issued straight afterwards fails every time. The
/// ceiling is for a host too loaded to hit that.
const USER_BUS_WAIT: Duration = Duration::from_secs(10);
const USER_BUS_POLL: Duration = Duration::from_millis(50);

/// Which systemd manager the unit is written for.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Scope {
    User,
    System,
}

impl Scope {
    fn install_target(self) -> &'static str {
        match self {
            Scope::User => "default.target",
            Scope::System => "multi-user.target",
        }
    }
}

/// The enrollment invocation. The unit only retains the profile selector
/// because worker.toml holds the connection mode and durable credential.
#[derive(Clone, Debug, Default)]
pub struct Invocation {
    pub name: Option<String>,
    pub verbosity: u8,
}

impl Invocation {
    /// The argument vector `ExecStart` runs, without the binary itself.
    fn exec_args(&self) -> Vec<String> {
        let mut args: Vec<String> = Vec::new();
        if self.verbosity > 0 {
            args.push(format!("-{}", "v".repeat(self.verbosity as usize)));
        }
        args.push("worker".into());
        if let Some(name) = &self.name {
            args.push("--name".into());
            args.push(name.clone());
        }
        args
    }
}

/// Everything the unit text is derived from. Generation is a pure
/// function of this, so the paths are resolved by the caller rather than
/// read from the environment while the text is built.
#[derive(Clone, Debug)]
pub struct UnitSpec {
    pub scope: Scope,
    /// Absolute path of the `pm` that generated this, since a unit has no
    /// `PATH` worth relying on.
    pub exe: PathBuf,
    pub invocation: Invocation,
    /// Account the system unit runs as. Unused by a user unit, which
    /// already runs as its own user.
    pub user: String,
    pub home: PathBuf,
    /// `XDG_CONFIG_HOME` as the generator resolved it. The worker reads
    /// `worker.toml` under this, so a unit that does not pin it can look
    /// somewhere else than the shell that enrolled did.
    pub config_home: PathBuf,
    /// `XDG_DATA_HOME`, only when it was set. Left alone otherwise so the
    /// unit inherits whatever the manager's default is.
    pub data_home: Option<PathBuf>,
    /// Directories holding the agent binaries, resolved from the
    /// invoking `PATH`. The worker spawns agents by bare name, so a unit
    /// that cannot reach them starts cleanly and then fails every spawn.
    pub agent_dirs: Vec<PathBuf>,
}

impl UnitSpec {
    /// Whether the unit has to name the paths itself. A system unit runs
    /// under `User=` with no login environment, and a user manager rarely
    /// carries an `XDG_CONFIG_HOME` that was only ever set in a shell rc.
    fn pins_config_home(&self) -> bool {
        self.scope == Scope::System || self.config_home != self.home.join(".config")
    }
}

/// The unit's file name. A named worker gets `pm-worker@<name>.service`:
/// one machine can enroll several times, each with its own key, so each
/// needs its own file. These are concrete units rather than instances of
/// a template, because each carries its own `ExecStart`.
pub fn unit_file_name(name: Option<&str>) -> String {
    match name {
        Some(name) => format!("{UNIT_PREFIX}@{}.service", unit_instance(name)),
        None => format!("{UNIT_PREFIX}.service"),
    }
}

/// A worker name reduced to what a unit name may contain.
fn unit_instance(name: &str) -> String {
    name.chars()
        .map(|c| {
            if c.is_ascii_alphanumeric() || c == '-' || c == '_' || c == '.' {
                c
            } else {
                '-'
            }
        })
        .collect()
}

/// Where the unit file belongs for a scope.
pub fn unit_dir(scope: Scope, config_home: &Path) -> PathBuf {
    match scope {
        Scope::User => config_home.join("systemd").join("user"),
        Scope::System => PathBuf::from(SYSTEM_UNIT_DIR),
    }
}

pub fn unit_text(spec: &UnitSpec) -> String {
    let mut unit = String::new();
    unit.push_str(
        "# Generated by pm worker --systemd. Regenerate rather than drift.\n\
         # Connection settings and credentials are loaded from worker.toml.\n",
    );
    unit.push_str("\n[Unit]\n");
    unit.push_str(&format!("Description={}\n", description(spec)));
    unit.push_str("After=network-online.target\n");
    unit.push_str("Wants=network-online.target\n");

    unit.push_str("\n[Service]\n");
    unit.push_str("Type=simple\n");
    if spec.scope == Scope::System {
        unit.push_str(&format!("User={}\n", escape_value(&spec.user)));
        unit.push_str(&format!(
            "Environment=HOME={}\n",
            escape_value(&spec.home.display().to_string())
        ));
    }
    if spec.pins_config_home() {
        unit.push_str(&format!(
            "Environment=XDG_CONFIG_HOME={}\n",
            escape_value(&spec.config_home.display().to_string())
        ));
    }
    if let Some(data_home) = &spec.data_home {
        unit.push_str(&format!(
            "Environment=XDG_DATA_HOME={}\n",
            escape_value(&data_home.display().to_string())
        ));
    }
    if let Some(path) = agent_path(&spec.agent_dirs) {
        unit.push_str(&format!(
            "# PATH adds where the agent binaries were found when this unit was\n\
             # generated, since a manager with no login session has only {DEFAULT_PATH}.\n"
        ));
        unit.push_str(&format!("Environment=PATH={}\n", escape_value(&path)));
    }
    unit.push_str(&format!(
        "WorkingDirectory={}\n",
        escape_value(&spec.home.display().to_string())
    ));
    unit.push_str(&format!("ExecStart={}\n", exec_start(spec)));
    unit.push_str("Restart=always\n");
    unit.push_str(&format!("RestartSec={RESTART_SEC}\n"));

    unit.push_str("\n[Install]\n");
    unit.push_str(&format!("WantedBy={}\n", spec.scope.install_target()));
    unit
}

/// The unit's `PATH`: where the agents actually live, ahead of the
/// system directories. `None` when everything was already reachable, so
/// the unit does not narrow a manager's `PATH` for no reason.
fn agent_path(agent_dirs: &[PathBuf]) -> Option<String> {
    let base: Vec<&str> = DEFAULT_PATH.split(':').collect();
    let extra: Vec<String> = agent_dirs
        .iter()
        .map(|d| d.display().to_string())
        .filter(|d| !base.contains(&d.as_str()))
        .collect();
    if extra.is_empty() {
        return None;
    }
    Some(format!("{}:{DEFAULT_PATH}", extra.join(":")))
}

/// Directories holding the agents the worker can spawn, in the order the
/// invoking `PATH` lists them. Walking `SELECTABLE` rather than naming
/// agents keeps a new one from silently dropping out.
fn agent_dirs(path_var: &str, is_program: &dyn Fn(&Path) -> bool) -> Vec<PathBuf> {
    let entries: Vec<&str> = path_var
        .split(':')
        .filter(|entry| !entry.is_empty())
        .collect();
    let mut found: Vec<PathBuf> = Vec::new();
    for agent in pm_protocol::domain::AgentKind::SELECTABLE {
        for entry in &entries {
            let dir = Path::new(entry);
            if !dir.is_absolute() || !is_program(&dir.join(agent.program())) {
                continue;
            }
            let dir = dir.to_path_buf();
            if !found.contains(&dir) {
                found.push(dir);
            }
            break;
        }
    }
    found
}

/// Whether a path is a file this account could actually execute.
fn is_program(path: &Path) -> bool {
    use std::os::unix::fs::PermissionsExt;
    std::fs::metadata(path)
        .map(|m| m.is_file() && m.permissions().mode() & 0o111 != 0)
        .unwrap_or(false)
}

/// What the unit can reach. A worker whose manager cannot find an agent
/// starts cleanly and fails every spawn, so an install that found none
/// says so while the operator is still watching.
fn agent_path_note(agent_dirs: &[PathBuf]) -> String {
    let names: Vec<&str> = pm_protocol::domain::AgentKind::SELECTABLE
        .iter()
        .map(|a| a.program())
        .collect();
    match agent_path(agent_dirs) {
        None if agent_dirs.is_empty() => format!(
            "no agent binaries ({}) are on PATH, so this unit will start and \
             then fail every spawn. Install one and regenerate the unit",
            names.join(", ")
        ),
        None => {
            "the agents are already on a systemd manager's PATH, so the unit pins none".to_string()
        }
        Some(_) => format!(
            "the unit pins {} on PATH, where the agents were found",
            agent_dirs
                .iter()
                .map(|d| d.display().to_string())
                .collect::<Vec<_>>()
                .join(", ")
        ),
    }
}

fn description(spec: &UnitSpec) -> String {
    match &spec.invocation.name {
        Some(name) => format!("Puppet Master worker ({name})"),
        None => "Puppet Master worker".to_string(),
    }
}

fn exec_start(spec: &UnitSpec) -> String {
    let mut parts = vec![escape_value(&spec.exe.display().to_string())];
    parts.extend(spec.invocation.exec_args().iter().map(|a| escape_value(a)));
    parts.join(" ")
}

/// One `ExecStart` word as systemd will read it back. `%` introduces a
/// specifier, and a word with whitespace or quotes in it has to be
/// quoted or systemd splits it.
fn escape_value(raw: &str) -> String {
    let escaped = raw.replace('%', "%%");
    let needs_quotes = escaped.is_empty()
        || escaped
            .chars()
            .any(|c| c.is_whitespace() || c == '"' || c == '\'' || c == '\\');
    if !needs_quotes {
        return escaped;
    }
    let inner = escaped.replace('\\', "\\\\").replace('"', "\\\"");
    format!("\"{inner}\"")
}

/// Writes the unit, creating the directory when it is missing. Returns
/// the path written.
pub fn write_unit(dir: &Path, file_name: &str, text: &str) -> anyhow::Result<PathBuf> {
    std::fs::create_dir_all(dir)
        .with_context(|| format!("creating the unit directory {}", dir.display()))?;
    let path = dir.join(file_name);
    std::fs::write(&path, text).with_context(|| format!("writing {}", path.display()))?;
    Ok(path)
}

/// Whether the account's user manager runs without a login session. A
/// user unit on a host nobody stays logged in to needs this on, or it
/// stops at logout.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Linger {
    On,
    Off,
    Unknown,
}

/// Reads `Linger=` out of `loginctl show-user` output.
fn parse_linger(output: &str) -> Linger {
    for line in output.lines() {
        if let Some(value) = line.trim().strip_prefix("Linger=") {
            return match value.trim() {
                "yes" => Linger::On,
                "no" => Linger::Off,
                _ => Linger::Unknown,
            };
        }
    }
    Linger::Unknown
}

fn linger_state(user: &str) -> Linger {
    let out = std::process::Command::new("loginctl")
        .args(["show-user", user, "--property=Linger"])
        .output();
    match out {
        Ok(out) if out.status.success() => parse_linger(&String::from_utf8_lossy(&out.stdout)),
        _ => Linger::Unknown,
    }
}

fn enable_linger(user: &str) -> bool {
    std::process::Command::new("loginctl")
        .args(["enable-linger", user])
        .status()
        .map(|s| s.success())
        .unwrap_or(false)
}

/// What this run did about lingering, so a later failure can say which
/// of the account's settings it is responsible for.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
enum LingerAction {
    /// It was already on, so nothing here changed the account.
    AlreadyOn,
    EnabledNow,
    CouldNotEnable,
}

/// The three manager calls `--install` makes, in the order it makes them.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
enum Step {
    Reload,
    Enable,
    Restart,
}

const STEPS: [Step; 3] = [Step::Reload, Step::Enable, Step::Restart];

impl Step {
    fn args(self, file_name: &str) -> Vec<String> {
        match self {
            Step::Reload => vec!["daemon-reload".to_string()],
            Step::Enable => vec!["enable".to_string(), file_name.to_string()],
            Step::Restart => vec!["restart".to_string(), file_name.to_string()],
        }
    }

    fn label(self) -> &'static str {
        match self {
            Step::Reload => "reload",
            Step::Enable => "enable",
            Step::Restart => "start",
        }
    }
}

/// The `systemctl` invocation for a scope. `systemctl --user` finds the
/// manager's bus through `XDG_RUNTIME_DIR`, so pinning that one variable
/// is what lets an install run without a login session for the account.
fn systemctl_command(
    scope: Scope,
    runtime_dir: Option<&Path>,
    args: &[String],
) -> std::process::Command {
    let mut command = std::process::Command::new("systemctl");
    if scope == Scope::User {
        command.arg("--user");
        if let Some(dir) = runtime_dir {
            command.env("XDG_RUNTIME_DIR", dir);
        }
    }
    command.args(args);
    command
}

fn systemctl(scope: Scope, runtime_dir: Option<&Path>, args: &[String]) -> anyhow::Result<()> {
    let status = systemctl_command(scope, runtime_dir, args)
        .status()
        .context("running systemctl, which has to be on PATH to install a unit")?;
    if !status.success() {
        bail!("systemctl {} exited with {status}", args.join(" "));
    }
    Ok(())
}

fn resolve_runtime_dir() -> RuntimeDir {
    runtime_dir_from(
        std::env::var_os("XDG_RUNTIME_DIR").map(PathBuf::from),
        own_uid(),
    )
}

/// The runtime directory holding the account's user bus. An inherited
/// `XDG_RUNTIME_DIR` wins, because a session that set one knows better
/// than a path built from a uid. Without one there is no session for the
/// account, which is how a service account is normally set up, and
/// logind puts the directory at a known place for the uid.
fn runtime_dir_from(inherited: Option<PathBuf>, uid: u32) -> RuntimeDir {
    match inherited.filter(|p| p.is_absolute()) {
        Some(path) => RuntimeDir {
            path,
            resolved: false,
        },
        None => RuntimeDir {
            path: runtime_dir_for(uid),
            resolved: true,
        },
    }
}

/// Where the account's user bus lives, and whether this command had to
/// work that out rather than inherit it.
#[derive(Clone, Debug)]
struct RuntimeDir {
    path: PathBuf,
    /// Set when the environment carried no `XDG_RUNTIME_DIR`, which is
    /// also when the commands we print have to pin it themselves.
    resolved: bool,
}

fn runtime_dir_for(uid: u32) -> PathBuf {
    PathBuf::from(format!("{RUNTIME_DIR_ROOT}/{uid}"))
}

/// The real uid, matching the account `user_name` reports. A user unit
/// is always installed for the account pm is running as, so this is the
/// manager that reads the unit we just wrote.
fn own_uid() -> u32 {
    // SAFETY: getuid reads the caller's own credentials and cannot fail.
    unsafe { libc::getuid() }
}

/// Waits for the account's user manager to be reachable. Enabling
/// lingering starts it asynchronously, and the runtime directory is
/// created well before the bus socket inside it, so the directory
/// existing is not evidence that a reload will work.
fn wait_for_user_bus(runtime_dir: &Path, timeout: Duration) -> bool {
    let bus = runtime_dir.join(USER_BUS_SOCKET);
    let deadline = Instant::now() + timeout;
    loop {
        if bus.exists() {
            return true;
        }
        if Instant::now() >= deadline {
            return false;
        }
        std::thread::sleep(USER_BUS_POLL);
    }
}

/// Everything `--install` has already changed by the time something goes
/// wrong, which is what the operator needs in order to decide between
/// re-running, finishing by hand and undoing it.
#[derive(Clone, Debug)]
struct Installed {
    scope: Scope,
    unit_path: PathBuf,
    file_name: String,
    user: String,
    linger: Option<LingerAction>,
    runtime_dir: Option<RuntimeDir>,
    done: Vec<Step>,
}

impl Installed {
    fn remaining(&self) -> Vec<Step> {
        STEPS
            .iter()
            .copied()
            .filter(|step| !self.done.contains(step))
            .collect()
    }

    /// The `systemctl` command line for one step, written the way the
    /// operator has to type it: pinned to the account's bus when this
    /// command had to resolve it, and elevated for a system unit.
    fn command_line(&self, step: Step) -> String {
        let args = step.args(&self.file_name).join(" ");
        match self.scope {
            Scope::System => format!("sudo systemctl {args}"),
            Scope::User => match &self.runtime_dir {
                Some(dir) if dir.resolved => format!(
                    "XDG_RUNTIME_DIR={} systemctl --user {args}",
                    dir.path.display()
                ),
                _ => format!("systemctl --user {args}"),
            },
        }
    }
}

/// What a half-finished install has to say for itself. The unit file is
/// on disk and lingering may already be on, so an exit status alone
/// leaves the operator unable to tell whether to re-run or clean up.
fn unfinished_report(state: &Installed, cause: &str) -> String {
    let mut out = format!("{cause}\n\nthe unit is installed but not running.\n\nalready done:\n");
    out.push_str(&format!("  wrote {}\n", state.unit_path.display()));
    if state.linger == Some(LingerAction::EnabledNow) {
        out.push_str(&format!("  enabled lingering for {}\n", state.user));
    }
    for step in &state.done {
        out.push_str(&format!("  ran {}\n", state.command_line(*step)));
    }

    let remaining = state.remaining();
    out.push_str("\nstill to do: ");
    out.push_str(
        &remaining
            .iter()
            .map(|s| s.label())
            .collect::<Vec<_>>()
            .join(", "),
    );
    out.push_str(&format!(
        "\n\nfinish it{}:\n",
        match state.scope {
            Scope::User => format!(" as {}", state.user),
            Scope::System => String::new(),
        }
    ));
    for step in &remaining {
        out.push_str(&format!("  {}\n", state.command_line(*step)));
    }

    out.push_str("\nor undo it:\n");
    out.push_str(&format!("  rm {}\n", state.unit_path.display()));
    if state.linger == Some(LingerAction::EnabledNow) {
        out.push_str(&format!("  sudo loginctl disable-linger {}\n", state.user));
    }
    out
}

/// What the caller asked the generator to do.
pub struct Request {
    pub scope: Scope,
    pub install: bool,
    pub sandbox: bool,
    pub invocation: Invocation,
}

pub fn run(request: Request) -> anyhow::Result<()> {
    if !cfg!(target_os = "linux") {
        bail!(
            "--systemd generates systemd units, which only Linux has. \
             macOS is not covered"
        );
    }
    if request.sandbox {
        bail!(
            "--sandbox does not want a unit: the container runtime already \
             restarts the worker, and relaunching the launcher deletes the \
             container and provisions a new one. Pass --restart always to \
             `pm worker --sandbox` instead"
        );
    }

    let spec = resolve_spec(request.scope, request.invocation)?;
    let text = unit_text(&spec);
    let file_name = unit_file_name(spec.invocation.name.as_deref());
    eprintln!("{}", agent_path_note(&spec.agent_dirs));

    if !request.install {
        print!("{text}");
        eprintln!(
            "install this yourself, or rerun with --install to write \
             {} and enable it",
            unit_dir(spec.scope, &spec.config_home)
                .join(&file_name)
                .display()
        );
        return Ok(());
    }

    let dir = unit_dir(spec.scope, &spec.config_home);
    let path = write_unit(&dir, &file_name, &text)?;
    println!("wrote {}", path.display());

    let mut state = Installed {
        scope: spec.scope,
        unit_path: path,
        file_name: file_name.clone(),
        user: spec.user.clone(),
        linger: None,
        runtime_dir: None,
        done: Vec::new(),
    };

    if spec.scope == Scope::User {
        state.linger = Some(report_linger(&spec.user));
        let runtime_dir = resolve_runtime_dir();
        if !wait_for_user_bus(&runtime_dir.path, USER_BUS_WAIT) {
            let cause = format!(
                "{}'s user manager is not reachable: no bus at {} after waiting {}s",
                spec.user,
                runtime_dir.path.join(USER_BUS_SOCKET).display(),
                USER_BUS_WAIT.as_secs()
            );
            state.runtime_dir = Some(runtime_dir);
            bail!("{}", unfinished_report(&state, &cause));
        }
        state.runtime_dir = Some(runtime_dir);
    }

    for step in STEPS {
        let runtime_dir = state.runtime_dir.as_ref().map(|d| d.path.as_path());
        if let Err(err) = systemctl(spec.scope, runtime_dir, &step.args(&file_name)) {
            bail!("{}", unfinished_report(&state, &format!("{err:#}")));
        }
        state.done.push(step);
    }
    println!("enabled and started {file_name}");
    Ok(())
}

/// Makes sure the user manager outlives the operator's session, saying
/// exactly what to run when it cannot do that itself.
fn report_linger(user: &str) -> LingerAction {
    match linger_state(user) {
        Linger::On => LingerAction::AlreadyOn,
        Linger::Off | Linger::Unknown => {
            if enable_linger(user) {
                println!("enabled lingering for {user}, so the worker survives logout");
                return LingerAction::EnabledNow;
            }
            eprintln!(
                "lingering is not on for {user}, so this unit stops at logout. \
                 Enable it with: sudo loginctl enable-linger {user}"
            );
            LingerAction::CouldNotEnable
        }
    }
}

fn resolve_spec(scope: Scope, invocation: Invocation) -> anyhow::Result<UnitSpec> {
    let exe = std::env::current_exe().context("locating the running pm binary")?;
    let exe = std::fs::canonicalize(&exe).unwrap_or(exe);
    let home = home_dir()?;
    let config_home = std::env::var_os("XDG_CONFIG_HOME")
        .map(PathBuf::from)
        .filter(|p| p.is_absolute())
        .unwrap_or_else(|| home.join(".config"));
    let data_home = std::env::var_os("XDG_DATA_HOME")
        .map(PathBuf::from)
        .filter(|p| p.is_absolute());
    let path_var = std::env::var("PATH").unwrap_or_default();
    Ok(UnitSpec {
        scope,
        exe,
        invocation,
        user: user_name()?,
        home,
        config_home,
        data_home,
        agent_dirs: agent_dirs(&path_var, &is_program),
    })
}

fn home_dir() -> anyhow::Result<PathBuf> {
    directories::BaseDirs::new()
        .map(|dirs| dirs.home_dir().to_path_buf())
        .context("no home directory to resolve the worker's config against")
}

/// The account the system unit runs as. `id` answers from the real uid,
/// where `USER` is only inherited, so a sudo or a stale environment does
/// not put the wrong account in the unit.
fn user_name() -> anyhow::Result<String> {
    if let Ok(out) = std::process::Command::new("id").arg("-un").output() {
        let name = String::from_utf8_lossy(&out.stdout).trim().to_string();
        if out.status.success() && !name.is_empty() {
            return Ok(name);
        }
    }
    let name = std::env::var_os("USER")
        .or_else(|| std::env::var_os("LOGNAME"))
        .map(|n| n.to_string_lossy().to_string())
        .unwrap_or_default();
    if name.is_empty() {
        bail!("could not tell which user this is, so the unit has no User= to run as");
    }
    Ok(name)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn spec(scope: Scope, invocation: Invocation) -> UnitSpec {
        UnitSpec {
            scope,
            exe: PathBuf::from("/opt/pm/bin/pm"),
            invocation,
            user: "testuser".into(),
            home: PathBuf::from("/var/lib/pmworker"),
            config_home: PathBuf::from("/var/lib/pmworker/.config"),
            data_home: None,
            agent_dirs: Vec::new(),
        }
    }

    fn dialing() -> Invocation {
        Invocation::default()
    }

    #[test]
    fn an_unnamed_worker_gets_the_plain_unit_name() {
        assert_eq!(unit_file_name(None), "pm-worker.service");
    }

    /// One machine can enroll several times, each with its own key, so
    /// each name needs a file of its own.
    #[test]
    fn a_named_worker_gets_its_own_unit_file() {
        assert_eq!(unit_file_name(Some("build")), "pm-worker@build.service");
    }

    #[test]
    fn a_name_is_reduced_to_what_a_unit_name_allows() {
        assert_eq!(
            unit_file_name(Some("build/eu west")),
            "pm-worker@build-eu-west.service"
        );
    }

    /// The token is the whole reason the generator exists as more than a
    /// string join: it is accepted so the enrolling command works
    /// unchanged, and must not reach the unit.
    #[test]
    fn the_enrollment_token_never_reaches_the_unit() {
        let text = unit_text(&spec(Scope::User, dialing()));
        assert!(!text.contains("--token"), "{text}");
        assert!(!text.contains("secret"), "{text}");
    }

    #[test]
    fn exec_start_uses_the_saved_named_profile() {
        let text = unit_text(&spec(
            Scope::User,
            Invocation {
                name: Some("edge".into()),
                verbosity: 0,
            },
        ));
        assert!(
            text.contains("ExecStart=/opt/pm/bin/pm worker --name edge\n"),
            "{text}"
        );
        assert!(!text.contains("--listen"), "{text}");
        assert!(!text.contains("--allow-from"), "{text}");
    }

    #[test]
    fn the_default_profile_loads_its_connection_from_worker_config() {
        let text = unit_text(&spec(Scope::User, dialing()));
        assert!(text.contains("ExecStart=/opt/pm/bin/pm worker\n"), "{text}");
        assert!(!text.contains("--controller"), "{text}");
    }

    #[test]
    fn verbosity_is_reproduced_before_the_subcommand() {
        let text = unit_text(&spec(
            Scope::User,
            Invocation {
                verbosity: 2,
                ..dialing()
            },
        ));
        assert!(
            text.contains("ExecStart=/opt/pm/bin/pm -vv worker\n"),
            "{text}"
        );
    }

    /// A user unit runs as its own user under a manager that already has
    /// the right `HOME`, so pinning it would only go stale.
    #[test]
    fn a_user_unit_names_no_user_and_no_home() {
        let text = unit_text(&spec(Scope::User, dialing()));
        assert!(!text.contains("User="), "{text}");
        assert!(!text.contains("Environment=HOME="), "{text}");
        assert!(text.contains("WantedBy=default.target\n"), "{text}");
    }

    /// A system unit runs with no login environment at all, so the
    /// worker cannot find worker.toml unless the unit says where it is.
    #[test]
    fn a_system_unit_pins_the_user_and_its_config_paths() {
        let text = unit_text(&spec(Scope::System, dialing()));
        assert!(text.contains("User=testuser\n"), "{text}");
        assert!(
            text.contains("Environment=HOME=/var/lib/pmworker\n"),
            "{text}"
        );
        assert!(
            text.contains("Environment=XDG_CONFIG_HOME=/var/lib/pmworker/.config\n"),
            "{text}"
        );
        assert!(text.contains("WantedBy=multi-user.target\n"), "{text}");
    }

    /// The home is whatever was resolved, not `/home/<user>`.
    #[test]
    fn the_system_unit_uses_the_resolved_home_not_the_user_name() {
        let text = unit_text(&spec(Scope::System, dialing()));
        assert!(!text.contains("/home/testuser"), "{text}");
    }

    /// An `XDG_CONFIG_HOME` set only in a shell rc is not in the user
    /// manager's environment, so a user unit has to carry it too.
    #[test]
    fn a_user_unit_pins_a_relocated_config_home() {
        let mut spec = spec(Scope::User, dialing());
        spec.config_home = PathBuf::from("/srv/conf");
        let text = unit_text(&spec);
        assert!(
            text.contains("Environment=XDG_CONFIG_HOME=/srv/conf\n"),
            "{text}"
        );
    }

    #[test]
    fn a_data_home_is_pinned_only_when_it_was_set() {
        let text = unit_text(&spec(Scope::User, dialing()));
        assert!(!text.contains("XDG_DATA_HOME"), "{text}");

        let mut relocated = spec(Scope::User, dialing());
        relocated.data_home = Some(PathBuf::from("/srv/data"));
        assert!(
            unit_text(&relocated).contains("Environment=XDG_DATA_HOME=/srv/data\n"),
            "expected the relocated data home to be pinned"
        );
    }

    #[test]
    fn the_unit_restarts_the_worker() {
        let text = unit_text(&spec(Scope::User, dialing()));
        assert!(text.contains("Restart=always\n"), "{text}");
        assert!(text.contains("RestartSec=5\n"), "{text}");
    }

    /// systemd reads `%` as a specifier and splits on whitespace, so a
    /// name or path carrying either has to survive the round trip.
    #[test]
    fn exec_start_escapes_what_systemd_would_otherwise_read() {
        assert_eq!(escape_value("wss://host:7677"), "wss://host:7677");
        assert_eq!(escape_value("50%"), "50%%");
        assert_eq!(escape_value("build eu"), "\"build eu\"");
        assert_eq!(escape_value("a\"b"), "\"a\\\"b\"");
        assert_eq!(escape_value("a\\b"), "\"a\\\\b\"");
    }

    #[test]
    fn a_name_with_a_space_is_quoted_in_exec_start_but_slugged_in_the_file_name() {
        let text = unit_text(&spec(
            Scope::User,
            Invocation {
                name: Some("build eu".into()),
                ..dialing()
            },
        ));
        assert!(text.contains("--name \"build eu\""), "{text}");
        assert_eq!(
            unit_file_name(Some("build eu")),
            "pm-worker@build-eu.service"
        );
    }

    #[test]
    fn unit_directories_follow_the_scope() {
        let config = PathBuf::from("/home/testuser/.config");
        assert_eq!(
            unit_dir(Scope::User, &config),
            PathBuf::from("/home/testuser/.config/systemd/user")
        );
        assert_eq!(
            unit_dir(Scope::System, &config),
            PathBuf::from("/etc/systemd/system")
        );
    }

    /// What `--install` does to the filesystem, without enabling
    /// anything: the file lands under the scope's directory, named for
    /// the worker, and holds the unit that was printed.
    #[test]
    fn installing_writes_the_unit_where_the_scope_says() {
        let root = tempfile::tempdir().unwrap();
        let spec = spec(
            Scope::User,
            Invocation {
                name: Some("build".into()),
                ..dialing()
            },
        );
        let dir = unit_dir(Scope::User, root.path());
        let text = unit_text(&spec);
        let path = write_unit(
            &dir,
            &unit_file_name(spec.invocation.name.as_deref()),
            &text,
        )
        .unwrap();

        assert_eq!(
            path,
            root.path().join("systemd/user/pm-worker@build.service")
        );
        assert_eq!(std::fs::read_to_string(&path).unwrap(), text);
    }

    #[test]
    fn linger_is_read_out_of_loginctl() {
        assert_eq!(parse_linger("Linger=yes\n"), Linger::On);
        assert_eq!(parse_linger("Linger=no\n"), Linger::Off);
        assert_eq!(parse_linger(""), Linger::Unknown);
        assert_eq!(parse_linger("Something=else\n"), Linger::Unknown);
    }

    fn installed(scope: Scope) -> Installed {
        Installed {
            scope,
            unit_path: PathBuf::from("/home/svc/.config/systemd/user/pm-worker@build.service"),
            file_name: "pm-worker@build.service".into(),
            user: "svc".into(),
            linger: None,
            runtime_dir: None,
            done: Vec::new(),
        }
    }

    fn env_of(command: &std::process::Command) -> Vec<(String, Option<String>)> {
        command
            .get_envs()
            .map(|(k, v)| {
                (
                    k.to_string_lossy().to_string(),
                    v.map(|v| v.to_string_lossy().to_string()),
                )
            })
            .collect()
    }

    fn args_of(command: &std::process::Command) -> Vec<String> {
        command
            .get_args()
            .map(|a| a.to_string_lossy().to_string())
            .collect()
    }

    /// Installing for an account with no login session is how a service
    /// account is set up, and `systemctl --user` finds the manager's bus
    /// only through `XDG_RUNTIME_DIR`. Without it the call cannot reach
    /// any bus at all, which is the whole failure.
    #[test]
    fn a_user_call_pins_the_accounts_runtime_directory() {
        let dir = PathBuf::from("/run/user/1001");
        let command = systemctl_command(Scope::User, Some(&dir), &["daemon-reload".to_string()]);
        assert_eq!(args_of(&command), ["--user", "daemon-reload"]);
        assert_eq!(
            env_of(&command),
            [("XDG_RUNTIME_DIR".to_string(), Some("/run/user/1001".into()))]
        );
    }

    /// A system unit is the host manager's, reached over the system bus,
    /// so a runtime directory would be meaningless there.
    #[test]
    fn a_system_call_names_no_user_manager_and_no_runtime_directory() {
        let dir = PathBuf::from("/run/user/1001");
        let command = systemctl_command(
            Scope::System,
            Some(&dir),
            &["enable".to_string(), "pm-worker.service".to_string()],
        );
        assert_eq!(args_of(&command), ["enable", "pm-worker.service"]);
        assert!(env_of(&command).is_empty(), "{:?}", env_of(&command));
    }

    /// A session that set `XDG_RUNTIME_DIR` knows better than a path
    /// built from a uid, and the finishing commands need not repeat what
    /// the operator's environment already carries.
    #[test]
    fn an_inherited_runtime_directory_is_used_as_it_stands() {
        let resolved = runtime_dir_from(Some(PathBuf::from("/run/user/501")), 1001);
        assert_eq!(resolved.path, PathBuf::from("/run/user/501"));
        assert!(!resolved.resolved);
    }

    #[test]
    fn without_one_the_runtime_directory_comes_from_the_uid() {
        let resolved = runtime_dir_from(None, 1001);
        assert_eq!(resolved.path, PathBuf::from("/run/user/1001"));
        assert!(resolved.resolved);

        let relative = runtime_dir_from(Some(PathBuf::from("run/user/501")), 1001);
        assert_eq!(relative.path, PathBuf::from("/run/user/1001"));
        assert!(relative.resolved);
    }

    /// Enabling lingering starts the user manager asynchronously and the
    /// runtime directory is created before the bus socket inside it, so
    /// the directory existing is not evidence that a reload will work.
    #[test]
    fn the_wait_is_for_the_bus_socket_not_the_directory() {
        let root = tempfile::tempdir().unwrap();
        assert!(
            !wait_for_user_bus(root.path(), Duration::from_millis(120)),
            "an empty runtime directory must not count as a reachable manager"
        );

        std::os::unix::net::UnixListener::bind(root.path().join(USER_BUS_SOCKET)).unwrap();
        assert!(wait_for_user_bus(root.path(), Duration::from_millis(120)));
    }

    #[test]
    fn the_steps_are_reload_then_enable_then_start() {
        let names: Vec<_> = STEPS.iter().map(|s| s.label()).collect();
        assert_eq!(names, ["reload", "enable", "start"]);
        assert_eq!(Step::Reload.args("u.service"), ["daemon-reload"]);
        assert_eq!(Step::Enable.args("u.service"), ["enable", "u.service"]);
        assert_eq!(Step::Restart.args("u.service"), ["restart", "u.service"]);
    }

    /// The half-installed state is the part that strands an operator: the
    /// unit is written and lingering is on, so an exit status alone
    /// leaves them unable to tell whether to re-run, finish, or clean up.
    #[test]
    fn a_failed_install_names_everything_it_already_changed() {
        let mut state = installed(Scope::User);
        state.linger = Some(LingerAction::EnabledNow);
        state.runtime_dir = Some(runtime_dir_from(None, 1001));

        let report = unfinished_report(&state, "systemctl daemon-reload exited with 1");

        assert!(
            report.contains("systemctl daemon-reload exited with 1"),
            "{report}"
        );
        assert!(
            report.contains("wrote /home/svc/.config/systemd/user/pm-worker@build.service"),
            "{report}"
        );
        assert!(report.contains("enabled lingering for svc"), "{report}");
        assert!(
            report.contains("still to do: reload, enable, start"),
            "{report}"
        );
    }

    /// The commands have to be runnable as printed. Without a login
    /// session there is no `XDG_RUNTIME_DIR` to inherit, so the ones we
    /// hand over have to carry the same value this command resolved.
    #[test]
    fn the_finishing_commands_carry_the_resolved_runtime_directory() {
        let mut state = installed(Scope::User);
        state.linger = Some(LingerAction::EnabledNow);
        state.runtime_dir = Some(runtime_dir_from(None, 1001));

        let report = unfinished_report(&state, "boom");

        for args in [
            "daemon-reload",
            "enable pm-worker@build.service",
            "restart pm-worker@build.service",
        ] {
            assert!(
                report.contains(&format!(
                    "XDG_RUNTIME_DIR=/run/user/1001 systemctl --user {args}\n"
                )),
                "{report}"
            );
        }
        assert!(
            report.contains("rm /home/svc/.config/systemd/user/pm-worker@build.service"),
            "{report}"
        );
        assert!(
            report.contains("sudo loginctl disable-linger svc"),
            "{report}"
        );
    }

    /// An inherited runtime directory is already in the operator's
    /// environment, so repeating it would only be noise.
    #[test]
    fn an_inherited_runtime_directory_is_not_repeated_on_the_commands() {
        let mut state = installed(Scope::User);
        state.runtime_dir = Some(runtime_dir_from(Some(PathBuf::from("/run/user/501")), 501));

        let report = unfinished_report(&state, "boom");

        assert!(
            report.contains("  systemctl --user daemon-reload\n"),
            "{report}"
        );
        assert!(!report.contains("XDG_RUNTIME_DIR="), "{report}");
    }

    /// Steps that succeeded are the difference between "re-run it" and
    /// "finish it", so the report must not lump them in with the rest.
    #[test]
    fn a_step_that_already_ran_is_reported_as_done_not_remaining() {
        let mut state = installed(Scope::User);
        state.runtime_dir = Some(runtime_dir_from(None, 1001));
        state.done.push(Step::Reload);

        let report = unfinished_report(&state, "boom");

        assert!(
            report.contains("ran XDG_RUNTIME_DIR=/run/user/1001 systemctl --user daemon-reload"),
            "{report}"
        );
        assert!(report.contains("still to do: enable, start"), "{report}");
        assert!(
            !report.contains("  XDG_RUNTIME_DIR=/run/user/1001 systemctl --user daemon-reload\n"),
            "a completed step must not be offered again: {report}"
        );
    }

    /// Lingering this run found already on is not this run's to claim, or
    /// to offer to undo: turning it off could stop somebody else's units.
    #[test]
    fn lingering_that_was_already_on_is_neither_claimed_nor_offered_for_undo() {
        let mut state = installed(Scope::User);
        state.linger = Some(LingerAction::AlreadyOn);
        state.runtime_dir = Some(runtime_dir_from(None, 1001));

        let report = unfinished_report(&state, "boom");

        assert!(!report.contains("enabled lingering"), "{report}");
        assert!(!report.contains("disable-linger"), "{report}");
    }

    /// The worker spawns agents by bare name, so the unit's PATH is what
    /// decides whether a session can start at all. A manager with no
    /// login session carries only the system directories, which is why an
    /// agent under the operator's home is invisible to it.
    #[test]
    fn the_directory_holding_an_agent_is_put_ahead_of_the_system_path() {
        let dirs = vec![PathBuf::from("/home/svc/.local/bin")];
        assert_eq!(
            agent_path(&dirs).as_deref(),
            Some(
                "/home/svc/.local/bin:/usr/local/sbin:/usr/local/bin:/usr/sbin:/usr/bin:/sbin:/bin"
            )
        );

        let mut spec = spec(Scope::User, dialing());
        spec.agent_dirs = dirs;
        let text = unit_text(&spec);
        assert!(
            text.contains("Environment=PATH=/home/svc/.local/bin:/usr/local/sbin:"),
            "{text}"
        );
    }

    /// Pinning a PATH that adds nothing would only narrow what the
    /// manager already had, dropping entries such as /snap/bin.
    #[test]
    fn a_unit_pins_no_path_when_the_agents_are_already_reachable() {
        assert_eq!(agent_path(&[]), None);
        assert_eq!(agent_path(&[PathBuf::from("/usr/local/bin")]), None);
        assert!(!unit_text(&spec(Scope::User, dialing())).contains("PATH="));
    }

    /// A reader of the unit has to be able to tell where the PATH came
    /// from, or it is just an unexplained string in a long-lived file.
    #[test]
    fn the_unit_says_where_its_path_came_from() {
        let mut spec = spec(Scope::User, dialing());
        spec.agent_dirs = vec![PathBuf::from("/home/svc/.local/bin")];
        let text = unit_text(&spec);
        let path_line = text
            .lines()
            .position(|l| l.starts_with("Environment=PATH="))
            .expect("a PATH line");
        assert!(
            text.lines().nth(path_line - 1).unwrap().starts_with('#'),
            "the PATH line is unexplained: {text}"
        );
        assert!(
            text.contains("# PATH adds where the agent binaries were found"),
            "{text}"
        );
    }

    fn fake_tree(present: &[&str]) -> impl Fn(&Path) -> bool + use<> {
        let present: Vec<String> = present.iter().map(|p| p.to_string()).collect();
        move |path: &Path| present.contains(&path.display().to_string())
    }

    /// Only directories that actually hold an agent are pinned. Capturing
    /// the whole invoking PATH would carry build and version-manager
    /// directories into a unit that outlives the shell that made it.
    #[test]
    fn only_directories_holding_an_agent_are_taken_from_the_invoking_path() {
        let dirs = agent_dirs(
            "/home/svc/build/target/debug:/home/svc/.local/bin:/usr/bin",
            &fake_tree(&["/home/svc/.local/bin/claude"]),
        );
        assert_eq!(dirs, vec![PathBuf::from("/home/svc/.local/bin")]);
    }

    /// A machine can have the agents spread across directories, and the
    /// earliest match wins per agent, exactly as a PATH lookup would.
    #[test]
    fn each_agent_contributes_the_directory_that_would_win_a_path_lookup() {
        let dirs = agent_dirs(
            "/opt/a:/opt/b:/opt/c",
            &fake_tree(&[
                "/opt/b/claude",
                "/opt/a/claude",
                "/opt/c/codex",
                "/opt/b/gemini",
            ]),
        );
        assert_eq!(
            dirs,
            vec![
                PathBuf::from("/opt/a"),
                PathBuf::from("/opt/c"),
                PathBuf::from("/opt/b"),
            ],
            "claude resolves to /opt/a because it comes first on PATH"
        );
    }

    /// A relative PATH entry cannot mean anything to a unit, which runs
    /// from a working directory the operator's shell never had.
    #[test]
    fn relative_and_empty_path_entries_are_ignored() {
        assert!(agent_dirs("", &fake_tree(&["claude"])).is_empty());
        assert!(agent_dirs(".:bin::", &fake_tree(&["./claude", "bin/claude"])).is_empty());
    }

    /// An install that finds no agent produces a unit that starts cleanly
    /// and then fails every spawn, which is the shape of failure the
    /// operator cannot see. It has to be said out loud.
    #[test]
    fn finding_no_agent_at_all_is_reported_rather_than_left_silent() {
        let note = agent_path_note(&[]);
        assert!(note.contains("fail every spawn"), "{note}");
        for agent in ["claude", "codex", "gemini", "opencode", "agy"] {
            assert!(note.contains(agent), "{note}");
        }

        let found = agent_path_note(&[PathBuf::from("/home/svc/.local/bin")]);
        assert!(found.contains("/home/svc/.local/bin"), "{found}");
        assert!(!found.contains("fail every spawn"), "{found}");
    }

    /// A system unit has no lingering and no user manager, so its
    /// finishing commands are the elevated system ones.
    #[test]
    fn a_system_install_hands_over_elevated_commands() {
        let mut state = installed(Scope::System);
        state.unit_path = PathBuf::from("/etc/systemd/system/pm-worker@build.service");

        let report = unfinished_report(&state, "boom");

        assert!(
            report.contains("  sudo systemctl daemon-reload\n"),
            "{report}"
        );
        assert!(
            report.contains("  sudo systemctl enable pm-worker@build.service\n"),
            "{report}"
        );
        assert!(!report.contains("--user"), "{report}");
        assert!(!report.contains("linger"), "{report}");
        assert!(
            report.contains("rm /etc/systemd/system/pm-worker@build.service"),
            "{report}"
        );
    }
}
