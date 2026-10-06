//! `pm worker`'s own subcommands, and the rule that makes a worker's
//! stored settings the one description of it.
//!
//! A worker is named locally and its profile records how it runs. The
//! run path therefore launches from the profile rather than from the
//! command line, and a flag that would change the profile is refused
//! with the command that changes it. Without that, re-running with one
//! `--dir` silently dropped every other setting the container had.

use anyhow::{bail, Context};
use std::io::{BufRead, IsTerminal, Write};

use crate::sandbox;
use crate::worker;

/// Flags on a run that describe the container rather than the run. Each
/// is settable with `pm worker modify`, and passing one to a worker that
/// is already configured is refused rather than applied for one launch.
pub struct ConfigFlags<'a> {
    pub dirs: &'a [String],
    pub env: &'a [String],
    pub network: Option<&'a str>,
    pub restart: Option<sandbox::RestartPolicy>,
    pub image: Option<&'a str>,
    pub runtime: Option<sandbox::RuntimeKind>,
    pub incus_shifted_home: Option<bool>,
    pub limits: &'a sandbox::SandboxLimits,
    pub sandbox: bool,
    pub controller: Option<&'a str>,
    pub listen: bool,
}

/// What the worker already holds, so a flag that repeats it can be told
/// apart from one that changes it. `pm worker --sandbox` bakes the
/// container's command line, including `--controller`, and the container
/// re-runs it on every restart with the enrolling token long since spent:
/// comparing values is what keeps that from reading as a settings change.
#[derive(Default)]
pub struct Stored<'a> {
    pub sandbox: Option<&'a worker::SandboxProfile>,
    pub controller: Option<&'a str>,
}

impl ConfigFlags<'_> {
    /// Those of them that only mean anything to a container, so a
    /// worker without one can say so rather than talking about
    /// settings it could never hold.
    fn container_only(&self, stored: &Stored<'_>) -> Vec<&'static str> {
        self.named(stored)
            .into_iter()
            .filter(|flag| !matches!(*flag, "--controller" | "--listen"))
            .collect()
    }

    /// Which of them were actually passed, by the name an operator
    /// typed, so a refusal can name them back.
    fn named(&self, stored: &Stored<'_>) -> Vec<&'static str> {
        let held = stored.sandbox;
        let changes = |same: bool| !same;
        let mut out = Vec::new();
        if !self.dirs.is_empty() && changes(held.is_some_and(|h| h.dirs == self.dirs)) {
            out.push("--dir");
        }
        if !self.env.is_empty() && changes(held.is_some_and(|h| h.env == self.env)) {
            out.push("--env");
        }
        if let Some(network) = self.network {
            if changes(held.is_some_and(|h| h.network.as_deref() == Some(network))) {
                out.push("--network");
            }
        }
        if let Some(restart) = self.restart {
            if changes(held.is_some_and(|h| h.restart == restart)) {
                out.push("--restart");
            }
        }
        if let Some(image) = self.image {
            if changes(held.is_some_and(|h| h.image.as_deref() == Some(image))) {
                out.push("--image");
            }
        }
        if let Some(runtime) = self.runtime {
            if changes(held.is_some_and(|h| h.runtime == Some(runtime))) {
                out.push("--runtime");
            }
        }
        if let Some(shifted) = self.incus_shifted_home {
            if changes(held.is_some_and(|h| h.incus_shifted_home == shifted)) {
                out.push("--incus-shifted-home");
            }
        }
        if !self.limits.is_unset() && changes(held.is_some_and(|h| &h.limits == self.limits)) {
            out.push("a resource cap");
        }
        if let Some(controller) = self.controller {
            if changes(stored.controller == Some(controller)) {
                out.push("--controller");
            }
        }
        if self.listen {
            out.push("--listen");
        }
        out
    }
}

/// A container setting passed to a run that builds no container. Before
/// profiles this was clap's job, with `requires = "sandbox"` on each
/// flag, but whether a worker gets a container is now its profile's
/// answer and clap cannot see a profile.
pub fn refuse_container_flags_without_a_container(
    profile: &str,
    flags: &ConfigFlags<'_>,
    stored: &Stored<'_>,
) -> anyhow::Result<()> {
    let passed = flags.container_only(stored);
    if passed.is_empty() {
        return Ok(());
    }
    bail!(
        "{} only {} a worker in a container: pass --sandbox to give worker {profile} one, \
         or name a worker whose profile records one",
        passed.join(", "),
        if passed.len() > 1 {
            "apply to"
        } else {
            "applies to"
        }
    )
}

/// Whether a run may carry configuration flags.
///
/// A first run is how a worker is configured at all, and re-enrollment
/// legitimately re-passes the controller and the token together, so both
/// are allowed through. Anything else aimed at an already-configured
/// worker is refused: applying it for one launch would leave the profile
/// describing something the container is not.
pub fn refuse_config_flags(
    profile: &str,
    configured: bool,
    containerized: bool,
    enrolling: bool,
    flags: &ConfigFlags<'_>,
    stored: &Stored<'_>,
) -> anyhow::Result<()> {
    if !configured || enrolling {
        return Ok(());
    }
    // A worker with no container has nowhere to put a container
    // setting, and moving it into one is not an edit: the enrollment a
    // container needs lives inside it.
    if !containerized {
        let container_only = flags.container_only(stored);
        if flags.sandbox || !container_only.is_empty() {
            let (subject, verb) = if flags.sandbox {
                ("--sandbox".to_string(), "does not apply")
            } else if container_only.len() > 1 {
                (container_only.join(", "), "do not apply")
            } else {
                (container_only.join(", "), "does not apply")
            };
            bail!(
                "worker {profile} runs on this machine, not in a container, so {subject} \
                 {verb} to it. A container is a separate worker with its own enrollment, \
                 so give it a name of its own:\n  \
                 pm worker --sandbox --name <name> --controller <url> --token <enrollment>"
            );
        }
    }
    let passed = flags.named(stored);
    if passed.is_empty() {
        return Ok(());
    }
    bail!(
        "worker {profile} is already configured, and {} {} its settings rather than this run.\n  \
         change it:  pm worker modify --name {profile} …\n  \
         apply it:   pm worker restart --name {profile}\n  \
         see it:     pm worker list",
        passed.join(", "),
        if passed.len() == 1 {
            "describes"
        } else {
            "describe"
        }
    )
}

/// The container settings a run uses: the stored ones, or the flags when
/// this run is what configures the worker.
pub fn resolve_sandbox(
    stored: Option<worker::SandboxProfile>,
    flags: &ConfigFlags<'_>,
) -> worker::SandboxProfile {
    if let Some(stored) = stored {
        return stored;
    }
    worker::SandboxProfile {
        runtime: flags.runtime,
        incus_shifted_home: flags.incus_shifted_home.unwrap_or_default(),
        image: flags.image.map(str::to_string),
        restart: flags.restart.unwrap_or_default(),
        network: flags.network.map(str::to_string),
        dirs: flags.dirs.to_vec(),
        env: flags.env.to_vec(),
        limits: flags.limits.clone(),
    }
}

/// What `pm worker list` prints for one worker.
fn summary_line(summary: &worker::ProfileSummary) -> String {
    let where_it_runs = match &summary.sandbox {
        Some(sandbox) => {
            let runtime = sandbox
                .runtime
                .map(|kind| kind.command().to_string())
                .unwrap_or_else(|| "container".to_string());
            format!("{runtime} {}", sandbox::container_name_of(&summary.name))
        }
        None => "this machine".to_string(),
    };
    let controller = summary
        .controller
        .clone()
        .unwrap_or_else(|| "not enrolled".to_string());
    let mut line = format!(
        "{}\n  runs in     {where_it_runs}\n  controller  {controller}",
        summary.name
    );
    if !summary.enrolled {
        line.push_str("\n  enrollment  none yet");
    }
    if let Some(sandbox) = &summary.sandbox {
        line.push_str(&format!("\n  restart     {}", sandbox.restart.as_word()));
        if sandbox.runtime == Some(sandbox::RuntimeKind::Incus) || sandbox.incus_shifted_home {
            line.push_str(&format!("\n  shifted home {}", sandbox.incus_shifted_home));
        }
        if !sandbox.dirs.is_empty() {
            line.push_str(&format!("\n  dirs        {}", sandbox.dirs.join(", ")));
        }
        if !sandbox.env.is_empty() {
            line.push_str(&format!("\n  env         {}", sandbox.env.join(", ")));
        }
        if let Some(network) = &sandbox.network {
            line.push_str(&format!("\n  network     {network}"));
        }
        if let Some(image) = &sandbox.image {
            line.push_str(&format!("\n  image       {image}"));
        }
    }
    line
}

fn delete_description(profile: &str, sandboxed: bool, keep_resources: bool) -> String {
    let mut description =
        format!("Delete worker {profile} and its local configuration and enrollment.\n");
    if sandboxed && !keep_resources {
        description.push_str("This stops and removes its container and deletes its persistent home volume, including agent state. This cannot be undone. Shared images and bind-mounted host directories are kept.\n");
    } else if sandboxed {
        description.push_str("Its container and persistent agent state will be kept.\n");
    } else {
        description.push_str(
            "Files on this machine are kept. Its running worker process is not stopped.\n",
        );
    }
    description
}

fn confirm_delete(
    input: &mut impl BufRead,
    output: &mut impl Write,
    description: &str,
    interactive: bool,
    accepted: bool,
) -> anyhow::Result<bool> {
    write!(output, "{description}")?;
    if accepted {
        return Ok(true);
    }
    if !interactive {
        bail!(
            "worker deletion requires confirmation: pass --accept-delete to run without a terminal"
        );
    }
    write!(output, "Continue? [y/N] ")?;
    output.flush()?;
    let mut answer = String::new();
    input.read_line(&mut answer)?;
    Ok(matches!(
        answer.trim().to_ascii_lowercase().as_str(),
        "y" | "yes"
    ))
}

/// One `--dir`-style mutation applied to a stored list: a replacement
/// wins outright, then additions and removals adjust what is left.
pub fn apply_dirs(
    current: &[String],
    replace: &[String],
    add: &[String],
    remove: &[String],
) -> Vec<String> {
    let mut dirs: Vec<String> = if replace.is_empty() {
        current.to_vec()
    } else {
        replace.to_vec()
    };
    for spec in add {
        if !dirs.contains(spec) {
            dirs.push(spec.clone());
        }
    }
    // A removal names a host path, which is the part before any colon,
    // so it matches however the mount was spelled.
    dirs.retain(|spec| {
        let host = spec.split(':').next().unwrap_or(spec);
        !remove.iter().any(|target| target == host || target == spec)
    });
    dirs
}

#[allow(clippy::too_many_arguments)]
pub async fn run(cmd: crate::WorkerCmd, socket: &std::path::Path) -> anyhow::Result<()> {
    match cmd {
        crate::WorkerCmd::List => {
            let summaries = worker::summaries();
            if summaries.is_empty() {
                println!(
                    "no workers configured on this machine. Enroll one with the command \
                     the Hosts page shows."
                );
                return Ok(());
            }
            for (index, summary) in summaries.iter().enumerate() {
                if index > 0 {
                    println!();
                }
                println!("{}", summary_line(summary));
            }
            Ok(())
        }
        crate::WorkerCmd::Modify {
            name,
            dir,
            add_dir,
            rm_dir,
            env,
            restart,
            image,
            network,
            runtime,
            incus_shifted_home,
            limits,
        } => {
            let profile = worker::select(name.as_deref())?;
            if !worker::exists(&profile) {
                bail!("no worker called {profile} is configured on this machine");
            }
            let Some(mut sandbox) = worker::sandbox_of(&profile) else {
                bail!(
                    "worker {profile} runs on this machine, not in a container, so it has \
                     no container settings to change"
                );
            };
            sandbox.dirs = apply_dirs(&sandbox.dirs, &dir, &add_dir, &rm_dir);
            if !env.is_empty() {
                sandbox.env = env;
            }
            if let Some(restart) = restart {
                sandbox.restart = restart;
            }
            if image.is_some() {
                sandbox.image = image;
            }
            if network.is_some() {
                sandbox.network = network;
            }
            if runtime.is_some() {
                sandbox.runtime = runtime;
            }
            if let Some(shifted) = incus_shifted_home {
                sandbox.incus_shifted_home = shifted;
            }
            sandbox::validate_incus_shifted_home(sandbox.runtime, sandbox.incus_shifted_home)?;
            // Each cap is set only when it was passed, so changing one
            // leaves the others alone.
            let passed = sandbox::SandboxLimits::from(limits);
            if passed.cpu.is_some() {
                sandbox.limits.cpu = passed.cpu;
            }
            if passed.memory.is_some() {
                sandbox.limits.memory = passed.memory;
            }
            if passed.memory_swap.is_some() {
                sandbox.limits.memory_swap = passed.memory_swap;
            }
            if passed.cpu_allowance.is_some() {
                sandbox.limits.cpu_allowance = passed.cpu_allowance;
            }
            if passed.memory_enforce.is_some() {
                sandbox.limits.memory_enforce = passed.memory_enforce;
            }
            if passed.disk.is_some() {
                sandbox.limits.disk = passed.disk;
            }
            worker::set_sandbox(&profile, Some(sandbox))?;
            println!("worker {profile} updated. Apply it with:");
            println!("  pm worker restart --name {profile}");
            Ok(())
        }
        crate::WorkerCmd::Restart { name, force } => {
            let profile = worker::select(name.as_deref())?;
            let Some(stored) = worker::sandbox_of(&profile) else {
                bail!(
                    "worker {profile} runs on this machine, so there is no container to \
                     rebuild: its settings apply the next time you run `pm worker --name \
                     {profile}`"
                );
            };
            sandbox::relaunch(&profile, stored, force, socket).await
        }
        crate::WorkerCmd::Stop { name } => {
            let profile = worker::select(name.as_deref())?;
            let runtime = worker::sandbox_of(&profile).and_then(|s| s.runtime);
            sandbox::manage(&profile, runtime, sandbox::ManageOp::Stop)
        }
        crate::WorkerCmd::Start { name } => {
            let profile = worker::select(name.as_deref())?;
            let runtime = worker::sandbox_of(&profile).and_then(|s| s.runtime);
            sandbox::manage(&profile, runtime, sandbox::ManageOp::Start)
        }
        crate::WorkerCmd::Logs { name, follow } => {
            let profile = worker::select(name.as_deref())?;
            let runtime = worker::sandbox_of(&profile).and_then(|s| s.runtime);
            sandbox::manage(&profile, runtime, sandbox::ManageOp::Logs { follow })
        }
        crate::WorkerCmd::Delete {
            name,
            no_delete_resources,
            accept_delete,
        } => {
            let profile = worker::select(name.as_deref())?;
            if !worker::exists(&profile) {
                println!("no worker called {profile} was configured");
                return Ok(());
            }
            let sandboxed = worker::sandbox_of(&profile);
            let plan = delete_description(&profile, sandboxed.is_some(), no_delete_resources);
            let stdin = std::io::stdin();
            if !confirm_delete(
                &mut stdin.lock(),
                &mut std::io::stderr().lock(),
                &plan,
                stdin.is_terminal(),
                accept_delete,
            )? {
                println!("worker {profile} was not deleted");
                return Ok(());
            }
            let removed = if !no_delete_resources {
                if let Some(stored) = &sandboxed {
                    sandbox::delete_resources(&profile, stored.runtime).with_context(|| {
                        format!(
                            "resource cleanup failed for {profile}, local configuration was kept"
                        )
                    })?
                } else {
                    Vec::new()
                }
            } else {
                Vec::new()
            };
            worker::delete_profile(&profile)?;
            println!("deleted worker {profile}");
            for item in &removed {
                println!("removed {item}");
            }
            if sandboxed.is_some() && no_delete_resources {
                println!("its container and persistent agent state were kept");
            }
            Ok(())
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn no_flags<'a>(limits: &'a sandbox::SandboxLimits) -> ConfigFlags<'a> {
        ConfigFlags {
            dirs: &[],
            env: &[],
            network: None,
            restart: None,
            image: None,
            runtime: None,
            incus_shifted_home: None,
            limits,
            sandbox: false,
            controller: None,
            listen: false,
        }
    }

    fn held<'a>(sandbox: &'a worker::SandboxProfile, controller: &'a str) -> Stored<'a> {
        Stored {
            sandbox: Some(sandbox),
            controller: Some(controller),
        }
    }

    #[test]
    fn deletion_confirmation_requires_explicit_consent() {
        for (answer, expected) in [
            ("y\n", true),
            (" YES \n", true),
            ("n\n", false),
            ("\n", false),
            ("", false),
        ] {
            let mut output = Vec::new();
            assert_eq!(
                confirm_delete(
                    &mut answer.as_bytes(),
                    &mut output,
                    "delete plan\n",
                    true,
                    false
                )
                .unwrap(),
                expected
            );
            assert!(String::from_utf8(output).unwrap().contains("[y/N]"));
        }
        assert!(confirm_delete(
            &mut "yes\n".as_bytes(),
            &mut Vec::new(),
            "plan",
            false,
            false
        )
        .unwrap_err()
        .to_string()
        .contains("--accept-delete"));
        let mut output = Vec::new();
        assert!(confirm_delete(&mut "".as_bytes(), &mut output, "plan", false, true).unwrap());
        assert_eq!(String::from_utf8(output).unwrap(), "plan");
    }

    #[test]
    fn deletion_plan_describes_cleanup_and_preservation() {
        let cleanup = delete_description("build", true, false);
        assert!(
            cleanup.contains("container")
                && cleanup.contains("persistent home volume")
                && cleanup.contains("cannot be undone")
        );
        assert!(cleanup.contains("bind-mounted host directories are kept"));
        let keep = delete_description("build", true, true);
        assert!(keep.contains("will be kept"));
        assert!(!keep.contains("cannot be undone"));
        let machine = delete_description("build", false, false);
        assert!(machine.contains("Files on this machine are kept"));
        assert!(!machine.contains("volume"));
    }

    #[test]
    fn shifted_home_is_stored_and_changes_require_modify() {
        let limits = sandbox::SandboxLimits::default();
        let mut flags = no_flags(&limits);
        assert!(!resolve_sandbox(None, &flags).incus_shifted_home);
        flags.incus_shifted_home = Some(true);
        let profile = resolve_sandbox(None, &flags);
        assert!(profile.incus_shifted_home);
        let stored = held(&profile, "controller");
        refuse_config_flags("repos", true, true, false, &flags, &stored).unwrap();
        flags.incus_shifted_home = Some(false);
        assert!(
            refuse_config_flags("repos", true, true, false, &flags, &stored)
                .unwrap_err()
                .to_string()
                .contains("--incus-shifted-home")
        );
        assert!(resolve_sandbox(Some(profile), &flags).incus_shifted_home);
        assert!(
            refuse_container_flags_without_a_container("repos", &flags, &Stored::default())
                .is_err()
        );
    }

    #[test]
    fn a_container_restarting_on_its_own_controller_is_not_a_change() {
        let limits = sandbox::SandboxLimits::default();
        let profile = worker::SandboxProfile::default();
        let mut flags = no_flags(&limits);
        flags.controller = Some("wss://host:7677");
        refuse_config_flags(
            "repos",
            true,
            true,
            false,
            &flags,
            &held(&profile, "wss://host:7677"),
        )
        .expect("the same controller it is enrolled with");
    }

    /// A different controller is still a settings change, which is the
    /// case the refusal was written for.
    #[test]
    fn a_different_controller_is_still_refused() {
        let limits = sandbox::SandboxLimits::default();
        let profile = worker::SandboxProfile::default();
        let mut flags = no_flags(&limits);
        flags.controller = Some("wss://elsewhere:7677");
        let error = refuse_config_flags(
            "repos",
            true,
            true,
            false,
            &flags,
            &held(&profile, "wss://host:7677"),
        )
        .expect_err("a different controller changes the profile");
        assert!(error.to_string().contains("--controller"), "{error}");
    }

    /// Everything the container's line repeats is a repeat, not an edit,
    /// so a restart carrying the mounts and caps it already has goes
    /// through.
    #[test]
    fn a_restart_repeating_every_stored_setting_goes_through() {
        let limits = sandbox::SandboxLimits::default();
        let dirs = vec!["/srv/repos".to_string()];
        let env = vec!["TZ=UTC".to_string()];
        let profile = worker::SandboxProfile {
            dirs: dirs.clone(),
            env: env.clone(),
            network: Some("bridge".to_string()),
            restart: sandbox::RestartPolicy::UnlessStopped,
            ..Default::default()
        };
        let mut flags = no_flags(&limits);
        flags.dirs = &dirs;
        flags.env = &env;
        flags.network = Some("bridge");
        flags.restart = Some(sandbox::RestartPolicy::UnlessStopped);
        refuse_config_flags(
            "repos",
            true,
            true,
            false,
            &flags,
            &held(&profile, "wss://host:7677"),
        )
        .expect("every value matches what is stored");
    }

    /// Adding a mount to a worker that already has one is an edit, and the
    /// refusal still says which command makes it.
    #[test]
    fn adding_a_dir_to_a_configured_worker_still_points_at_modify() {
        let limits = sandbox::SandboxLimits::default();
        let profile = worker::SandboxProfile {
            dirs: vec!["/srv/repos".to_string()],
            ..Default::default()
        };
        let wanted = vec!["/srv/repos".to_string(), "/srv/extra".to_string()];
        let mut flags = no_flags(&limits);
        flags.dirs = &wanted;
        let error = refuse_config_flags(
            "repos",
            true,
            true,
            false,
            &flags,
            &held(&profile, "wss://host:7677"),
        )
        .expect_err("a new mount changes the profile");
        let text = error.to_string();
        assert!(text.contains("--dir"), "{text}");
        assert!(text.contains("pm worker modify --name repos"), "{text}");
    }

    /// A first run is how a worker gets configured, so it carries the
    /// settings. Refusing there would leave no way to set anything.
    #[test]
    fn a_first_run_may_carry_every_setting() {
        let limits = sandbox::SandboxLimits::default();
        let dirs = vec!["/srv/repos".to_string()];
        let mut flags = no_flags(&limits);
        flags.dirs = &dirs;
        flags.sandbox = true;
        refuse_config_flags("repos", false, false, false, &flags, &Stored::default())
            .expect("nothing is configured");
    }

    /// Re-enrollment re-passes the controller and the token together,
    /// and a host whose credential was rotated has to be able to.
    #[test]
    fn re_enrollment_may_carry_its_controller() {
        let limits = sandbox::SandboxLimits::default();
        let mut flags = no_flags(&limits);
        flags.controller = Some("wss://host:7677");
        refuse_config_flags("repos", true, true, true, &flags, &Stored::default())
            .expect("enrolling again");
        let error = refuse_config_flags("repos", true, true, false, &flags, &Stored::default())
            .expect_err("without a token it is a settings change");
        assert!(error.to_string().contains("--controller"), "{error}");
    }

    /// The point of the refusal: a run that carried one `--dir` used to
    /// silently drop every other setting the container had.
    #[test]
    fn a_setting_flag_on_a_configured_worker_names_itself_and_the_way_to_set_it() {
        let limits = sandbox::SandboxLimits::default();
        let dirs = vec!["/srv/new".to_string()];
        let mut flags = no_flags(&limits);
        flags.dirs = &dirs;
        flags.network = Some("bridge");
        let error = refuse_config_flags("repos", true, true, false, &flags, &Stored::default())
            .expect_err("refused");
        let text = error.to_string();
        assert!(text.contains("--dir"), "{text}");
        assert!(text.contains("--network"), "{text}");
        assert!(text.contains("pm worker modify --name repos"), "{text}");
        assert!(text.contains("pm worker restart --name repos"), "{text}");
    }

    /// A run with no settings on it is just a run, however configured
    /// the worker is.
    #[test]
    fn a_plain_run_is_never_refused() {
        let limits = sandbox::SandboxLimits::default();
        refuse_config_flags(
            "repos",
            true,
            true,
            false,
            &no_flags(&limits),
            &Stored::default(),
        )
        .expect("plain run");
    }

    /// A resource cap is a setting even though it is several flags.
    #[test]
    fn a_resource_cap_counts_as_a_setting() {
        let limits = sandbox::SandboxLimits {
            memory: Some("8GiB".into()),
            ..Default::default()
        };
        let error = refuse_config_flags(
            "repos",
            true,
            true,
            false,
            &no_flags(&limits),
            &Stored::default(),
        )
        .expect_err("refused");
        assert!(error.to_string().contains("a resource cap"), "{error}");
    }

    /// Moving a configured host worker into a container is not an edit:
    /// the enrollment it would need lives in the container's volume.
    #[test]
    fn sandboxing_a_configured_host_worker_is_refused_as_a_re_enrollment() {
        let limits = sandbox::SandboxLimits::default();
        let mut flags = no_flags(&limits);
        flags.sandbox = true;
        let error = refuse_config_flags("bench", true, false, false, &flags, &Stored::default())
            .expect_err("refused");
        let text = error.to_string();
        assert!(text.contains("--name <name>"), "{text}");
        // Forgetting it would destroy the enrollment of the worker that
        // is already there, which is never the answer to a name clash.
        assert!(!text.contains("forget"), "{text}");
        // Already containerized, so the flag only restates what it is.
        refuse_config_flags("repos", true, true, false, &flags, &Stored::default())
            .expect("already a container");
    }

    #[test]
    fn stored_settings_win_over_flags_once_they_exist() {
        let limits = sandbox::SandboxLimits::default();
        let dirs = vec!["/from/flags".to_string()];
        let mut flags = no_flags(&limits);
        flags.dirs = &dirs;
        let stored = worker::SandboxProfile {
            dirs: vec!["/from/profile".to_string()],
            ..Default::default()
        };
        let resolved = resolve_sandbox(Some(stored), &flags);
        assert_eq!(resolved.dirs, vec!["/from/profile".to_string()]);
        let fresh = resolve_sandbox(None, &flags);
        assert_eq!(fresh.dirs, vec!["/from/flags".to_string()]);
        assert_eq!(
            fresh.restart,
            sandbox::RestartPolicy::UnlessStopped,
            "an unset --restart takes the supervising default"
        );
    }

    /// `--dir` replaces, `--add-dir` and `--rm-dir` adjust. Adding what
    /// is already mounted is not an error and does not duplicate it.
    #[test]
    fn dir_edits_replace_add_and_remove() {
        let current = vec!["/a:ro".to_string(), "/b".to_string()];
        assert_eq!(
            apply_dirs(&current, &[], &["/c".to_string()], &[]),
            vec!["/a:ro", "/b", "/c"]
        );
        assert_eq!(
            apply_dirs(&current, &[], &["/a:ro".to_string()], &[]),
            current,
            "adding a mount it already has changes nothing"
        );
        assert_eq!(
            apply_dirs(&current, &[], &[], &["/a".to_string()]),
            vec!["/b"],
            "a removal names the host path, however the mount was spelled"
        );
        assert_eq!(
            apply_dirs(&current, &["/only".to_string()], &[], &[]),
            vec!["/only"],
            "--dir replaces outright"
        );
        assert_eq!(
            apply_dirs(
                &current,
                &["/x".to_string()],
                &["/y".to_string()],
                &["/x".to_string()]
            ),
            vec!["/y"],
            "a replacement is still subject to the adjustments"
        );
    }
}
