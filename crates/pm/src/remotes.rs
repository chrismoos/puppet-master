//! Which daemon a command talks to, and the commands that manage saved
//! logins to remote controllers.

use std::io::{BufRead, IsTerminal, Read, Write};
use std::path::{Path, PathBuf};

use pm_client::remote::{
    self, ControllerUrl, LoginRequest, RemoteStore, Revocation, TrustProbe, LOCAL_NAME,
};
use pm_client::{Remote, Target};
use pm_tls::{KeyHash, WebTrust};

pub const NAME_ARG_ID: &str = "controller_name";
const NAME_ARG_LONG: &str = "name";

/// Subcommands that never talk to a daemon chosen by name, so the flag
/// would be accepted and ignored on them.
const COMMANDS_WITHOUT_NAME_FLAG: &[&str] = &[
    "daemon", "pushgw", "worker", "update", "remotes", "_hook", "_mcp",
];

fn name_arg() -> clap::Arg {
    clap::Arg::new(NAME_ARG_ID)
        .long(NAME_ARG_LONG)
        .value_name("NAME")
        .help(
            "Controller to talk to: a name given to `pm login`, or `local` for the \
             daemon on this machine. Required when more than one is available",
        )
}

/// Adds `--name` to the command and to every subcommand under it, so the
/// flag is accepted wherever it is written. A subcommand that defines a
/// `--name` of its own keeps it, and the flag then goes before it.
pub fn with_name_flag(command: clap::Command) -> clap::Command {
    let has_own = command
        .get_arguments()
        .any(|arg| arg.get_long() == Some(NAME_ARG_LONG));
    if has_own || COMMANDS_WITHOUT_NAME_FLAG.contains(&command.get_name()) {
        return command;
    }
    command.arg(name_arg()).mut_subcommands(with_name_flag)
}

/// The `--name` written deepest in the command line, which is the one
/// closest to the command it applies to.
pub fn name_flag(matches: &clap::ArgMatches) -> Option<String> {
    let mut found = None;
    let mut level = matches;
    loop {
        if let Ok(Some(name)) = level.try_get_one::<String>(NAME_ARG_ID) {
            found = Some(name.clone());
        }
        match level.subcommand() {
            Some((_, sub)) => level = sub,
            None => return found,
        }
    }
}

/// How the socket path reached the command line, which decides whether
/// it can coexist with `--name`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SocketChoice {
    /// Neither `--socket` nor `PM_SOCKET`.
    Default,
    /// `PM_SOCKET`, which a session inherits rather than chooses.
    Environment,
    Flag,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Chosen {
    Local,
    Remote(String),
}

/// Picks the daemon a command talks to.
///
/// An explicit choice wins. Without one the command goes to the only
/// daemon there is, and refuses to guess between several: a command that
/// kills a session must not land on a controller its author did not mean.
/// `local_is_running` is only asked when the answer depends on it.
pub fn choose(
    name: Option<&str>,
    socket: SocketChoice,
    remotes: &[String],
    local_is_running: impl Fn() -> bool,
) -> Result<Chosen, String> {
    if let Some(name) = name {
        if socket == SocketChoice::Flag {
            return Err("--name and --socket each choose a daemon, pass only one".to_string());
        }
        if name == LOCAL_NAME {
            return Ok(Chosen::Local);
        }
        if remotes.iter().any(|remote| remote == name) {
            return Ok(Chosen::Remote(name.to_string()));
        }
        return Err(match remotes {
            [] => format!(
                "no saved login named {name:?}, sign in with `pm login <url> --name {name}`"
            ),
            known => format!(
                "no saved login named {name:?}, known names are: {}",
                known.join(", ")
            ),
        });
    }
    if socket != SocketChoice::Default {
        return Ok(Chosen::Local);
    }
    match remotes {
        [] => Ok(Chosen::Local),
        [only] if !local_is_running() => Ok(Chosen::Remote(only.clone())),
        [_] => Err(format!(
            "a daemon is running on this machine and a remote login is saved, \
             pass --name with one of: {LOCAL_NAME}, {}",
            remotes.join(", ")
        )),
        several => Err(format!(
            "several remote logins are saved, pass --name with one of: {}{}",
            if local_is_running() {
                format!("{LOCAL_NAME}, ")
            } else {
                String::new()
            },
            several.join(", ")
        )),
    }
}

pub fn store() -> RemoteStore {
    RemoteStore::new(crate::paths::remotes_dir())
}

/// Resolves the command line to the daemon it names.
pub fn target(
    name: Option<&str>,
    socket: SocketChoice,
    socket_path: PathBuf,
) -> anyhow::Result<Target> {
    let store = store();
    let remotes = store.names()?;
    let default_socket = crate::paths::default_socket_path();
    let chosen = choose(name, socket, &remotes, || local_is_running(&default_socket))
        .map_err(anyhow::Error::msg)?;
    Ok(match chosen {
        Chosen::Local => Target::Unix(socket_path),
        Chosen::Remote(name) => Target::Remote(Remote { store, name }),
    })
}

fn local_is_running(socket_path: &Path) -> bool {
    std::os::unix::net::UnixStream::connect(socket_path).is_ok()
}

/// The arguments that make a spawned `pm` reach the same daemon.
pub fn target_args(target: &Target) -> Vec<String> {
    match target {
        Target::Unix(path) => vec!["--socket".to_string(), path.display().to_string()],
        Target::Remote(remote) => vec![format!("--{NAME_ARG_LONG}"), remote.name.clone()],
    }
}

pub struct LoginArgs {
    pub url: String,
    pub name: Option<String>,
    pub username: Option<String>,
    pub password_stdin: bool,
    pub pin: Option<String>,
    pub device_name: Option<String>,
}

pub async fn login(args: LoginArgs) -> anyhow::Result<()> {
    let name = args
        .name
        .ok_or_else(|| anyhow::anyhow!("pass --name to name this controller, e.g. --name work"))?;
    remote::validate_name(&name)?;
    let url = ControllerUrl::parse(&args.url)?;
    let trust = decide_trust(&url, &name, args.pin.as_deref()).await?;

    let username = match args.username {
        Some(username) => username,
        None => prompt_line("Username: ")?,
    };
    let password = if args.password_stdin {
        let mut password = String::new();
        std::io::stdin().read_to_string(&mut password)?;
        password.trim_end_matches(['\r', '\n']).to_string()
    } else {
        prompt_password("Password: ")?
    };
    let device_name = args.device_name.unwrap_or_else(|| {
        format!(
            "pm on {}",
            pm_daemon::hostname::machine_hostname("this machine")
        )
    });

    let store = store();
    remote::login(
        &store,
        LoginRequest {
            name: &name,
            url: &url,
            trust,
            username: &username,
            password: &password,
            device_name: &device_name,
        },
    )
    .await?;
    println!(
        "signed in to {} as {username}, saved as {name:?}",
        url.http_base()
    );
    let others = store.names()?.len() > 1;
    if others || local_is_running(&crate::paths::default_socket_path()) {
        println!("more than one controller is now available, so commands need --name {name}");
    }
    Ok(())
}

async fn decide_trust(
    url: &ControllerUrl,
    name: &str,
    pin: Option<&str>,
) -> anyhow::Result<WebTrust> {
    if let Some(pin) = pin {
        let key = KeyHash::from_hex(pin.trim())
            .ok_or_else(|| anyhow::anyhow!("--pin takes the SHA-256 of a public key, in hex"))?;
        if !url.secure {
            anyhow::bail!("--pin applies to an https:// controller, and this URL is http://");
        }
        return Ok(WebTrust::Pinned(key));
    }
    match remote::probe_trust(url).await? {
        TrustProbe::Trusted => Ok(WebTrust::SystemRoots),
        TrustProbe::Plaintext => {
            if !url.is_loopback() {
                eprintln!(
                    "warning: {} is not encrypted. Your password and this login's tokens \
                     cross the network in the clear.",
                    url.http_base()
                );
            }
            Ok(WebTrust::SystemRoots)
        }
        TrustProbe::Untrusted { key } => {
            eprintln!(
                "The certificate at {} is not trusted by this system's root certificates.\n\
                 Its public key fingerprint (SHA-256) is:\n\n  {key}\n",
                url.http_base()
            );
            if !std::io::stdin().is_terminal() {
                anyhow::bail!(
                    "to trust that key without a prompt, pass --pin {key} after checking it"
                );
            }
            let answer = prompt_line(&format!(
                "Trust this key for {name:?} and refuse any other from now on? [y/N] "
            ))?;
            if !matches!(answer.as_str(), "y" | "Y" | "yes") {
                anyhow::bail!("not signed in: the certificate was not trusted");
            }
            Ok(WebTrust::Pinned(key))
        }
    }
}

/// One line from standard input, without its line ending.
fn read_answer(prompt: &str) -> anyhow::Result<String> {
    eprint!("{prompt}");
    std::io::stderr().flush()?;
    let mut line = String::new();
    if std::io::stdin().lock().read_line(&mut line)? == 0 {
        anyhow::bail!("no answer on standard input");
    }
    Ok(line.trim_end_matches(['\r', '\n']).to_string())
}

fn prompt_line(prompt: &str) -> anyhow::Result<String> {
    Ok(read_answer(prompt)?.trim().to_string())
}

/// Restores the terminal's echo when dropped, including on an early
/// return or a read error.
struct EchoOff {
    original: libc::termios,
}

impl EchoOff {
    fn enable() -> std::io::Result<Self> {
        // SAFETY: termios is plain data that tcgetattr fully initializes
        // on success, and failure is checked before it is read.
        unsafe {
            let mut original = std::mem::zeroed::<libc::termios>();
            if libc::tcgetattr(libc::STDIN_FILENO, &mut original) != 0 {
                return Err(std::io::Error::last_os_error());
            }
            let mut silent = original;
            silent.c_lflag &= !libc::ECHO;
            if libc::tcsetattr(libc::STDIN_FILENO, libc::TCSANOW, &silent) != 0 {
                return Err(std::io::Error::last_os_error());
            }
            Ok(Self { original })
        }
    }
}

impl Drop for EchoOff {
    fn drop(&mut self) {
        // SAFETY: restores the attributes tcgetattr returned for this fd.
        unsafe {
            libc::tcsetattr(libc::STDIN_FILENO, libc::TCSANOW, &self.original);
        }
    }
}

fn prompt_password(prompt: &str) -> anyhow::Result<String> {
    if !std::io::stdin().is_terminal() {
        anyhow::bail!("no terminal to ask for a password on, pass --password-stdin");
    }
    let echo_off = EchoOff::enable()?;
    let password = read_answer(prompt);
    drop(echo_off);
    eprintln!();
    password
}

/// The saved login a command without a daemon applies to: the named one,
/// or the only one.
fn named_or_only(store: &RemoteStore, name: Option<String>) -> anyhow::Result<String> {
    if let Some(name) = name {
        return Ok(name);
    }
    match store.names()?.as_slice() {
        [] => anyhow::bail!("no remote logins are saved"),
        [only] => Ok(only.clone()),
        several => anyhow::bail!("pass --name with one of: {}", several.join(", ")),
    }
}

pub async fn logout(name: Option<String>) -> anyhow::Result<()> {
    let store = store();
    let name = named_or_only(&store, name)?;
    match remote::logout(&store, &name).await? {
        Revocation::Revoked => println!("signed out of {name:?}"),
        Revocation::NotConfirmed => println!(
            "forgot the login for {name:?}. The controller did not confirm revoking it, \
             so remove the device in its user settings."
        ),
    }
    Ok(())
}

pub fn list() -> anyhow::Result<()> {
    let store = store();
    let names = store.names()?;
    if names.is_empty() {
        println!("no remote logins, sign in with `pm login <url> --name <name>`");
        return Ok(());
    }
    println!("{:<20} {:<40} USER", "NAME", "URL");
    for name in names {
        match store.load(&name) {
            Ok(profile) => println!("{:<20} {:<40} {}", name, profile.url, profile.username),
            Err(e) => println!("{name:<20} ({e})"),
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn names(names: &[&str]) -> Vec<String> {
        names.iter().map(|n| n.to_string()).collect()
    }

    fn never() -> bool {
        panic!("the local daemon must not be probed for this choice")
    }

    #[test]
    fn with_no_remote_logins_every_command_goes_to_the_local_daemon() {
        assert_eq!(
            choose(None, SocketChoice::Default, &[], never),
            Ok(Chosen::Local)
        );
    }

    #[test]
    fn a_single_remote_login_is_used_when_no_local_daemon_runs() {
        assert_eq!(
            choose(None, SocketChoice::Default, &names(&["work"]), || false),
            Ok(Chosen::Remote("work".into()))
        );
    }

    #[test]
    fn a_local_daemon_beside_a_remote_login_needs_a_name() {
        let error = choose(None, SocketChoice::Default, &names(&["work"]), || true).unwrap_err();
        assert!(error.contains("--name"), "{error}");
        assert!(error.contains("local, work"), "{error}");
    }

    #[test]
    fn several_remote_logins_need_a_name_and_list_local_only_when_it_runs() {
        let remotes = names(&["home", "work"]);
        let error = choose(None, SocketChoice::Default, &remotes, || false).unwrap_err();
        assert!(error.ends_with("one of: home, work"), "{error}");
        let error = choose(None, SocketChoice::Default, &remotes, || true).unwrap_err();
        assert!(error.ends_with("one of: local, home, work"), "{error}");
    }

    #[test]
    fn a_name_picks_that_login_or_the_local_daemon() {
        let remotes = names(&["home", "work"]);
        assert_eq!(
            choose(Some("work"), SocketChoice::Default, &remotes, never),
            Ok(Chosen::Remote("work".into()))
        );
        assert_eq!(
            choose(Some("local"), SocketChoice::Default, &remotes, never),
            Ok(Chosen::Local)
        );
        let error = choose(Some("nope"), SocketChoice::Default, &remotes, never).unwrap_err();
        assert!(error.contains("home, work"), "{error}");
        let error = choose(Some("nope"), SocketChoice::Default, &[], never).unwrap_err();
        assert!(error.contains("pm login"), "{error}");
    }

    #[test]
    fn an_explicit_socket_is_the_local_daemon_whatever_is_saved() {
        let remotes = names(&["home", "work"]);
        for socket in [SocketChoice::Flag, SocketChoice::Environment] {
            assert_eq!(choose(None, socket, &remotes, never), Ok(Chosen::Local));
        }
    }

    #[test]
    fn a_name_overrides_an_inherited_socket_but_not_one_passed_beside_it() {
        let remotes = names(&["work"]);
        assert_eq!(
            choose(Some("work"), SocketChoice::Environment, &remotes, never),
            Ok(Chosen::Remote("work".into()))
        );
        let error = choose(Some("work"), SocketChoice::Flag, &remotes, never).unwrap_err();
        assert!(error.contains("pass only one"), "{error}");
    }

    #[test]
    fn spawned_commands_are_pointed_at_the_same_daemon() {
        assert_eq!(
            target_args(&Target::Unix("/run/pm.sock".into())),
            ["--socket", "/run/pm.sock"]
        );
        let remote = Target::Remote(Remote {
            store: RemoteStore::new("/cfg".into()),
            name: "work".into(),
        });
        assert_eq!(target_args(&remote), ["--name", "work"]);
    }
}
