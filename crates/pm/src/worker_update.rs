//! Keeping a worker's build in step with its controller.
//!
//! The two ends must speak the same worker protocol to talk at all, so a
//! worker follows the build its controller reports rather than whatever
//! is newest. That includes going backwards: a controller that was rolled
//! back still has to be reachable.

use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};

use anyhow::Context as _;
use tracing::{error, info, warn};

use pm_daemon::update;

const IDLE_POLL: std::time::Duration = std::time::Duration::from_secs(20);

/// A build the worker should move to once it is safe to restart.
#[derive(Default)]
pub struct PendingUpdate {
    target: Mutex<Option<String>>,
    /// The controller's release, kept even when it matches this build, so
    /// a forced update still has something to install.
    controller_release: Mutex<Option<String>>,
    forced: AtomicBool,
}

impl PendingUpdate {
    /// Records the build the controller runs. Nothing happens if it is
    /// already the build in use.
    pub fn note_controller_build(&self, controller_version: &str) {
        let target = update::release_version(controller_version).to_string();
        *self.controller_release.lock().unwrap() = (!target.is_empty()).then(|| target.clone());
        if target.is_empty() || target == update::release_version(pm_daemon::pm_build_version()) {
            *self.target.lock().unwrap() = None;
            return;
        }
        let previous = self.target.lock().unwrap().replace(target.clone());
        if previous.as_deref() != Some(target.as_str()) {
            info!(
                target = %target,
                current = %update::release_version(pm_daemon::pm_build_version()),
                "controller runs a different pm build, updating when this worker is idle"
            );
        }
    }

    /// Drops the idle requirement. The operator has been told what an
    /// update costs the agents running here: they are killed and resumed
    /// afterwards, but an in-flight turn does not continue.
    pub fn force(&self) {
        self.forced.store(true, Ordering::SeqCst);
    }

    pub fn target(&self) -> Option<String> {
        self.target.lock().unwrap().clone()
    }

    fn is_forced(&self) -> bool {
        self.forced.load(Ordering::SeqCst)
    }

    /// What to install right now. Normally the pending target. An operator
    /// who forced an update gets the controller's release even when it
    /// matches this build's release: two builds of one release differ only
    /// in build metadata, which the release channel cannot address, so
    /// reinstalling that release is the only move available. It is also the
    /// only way back for a host the controller refuses on protocol.
    fn install_target(&self) -> Option<String> {
        if let Some(target) = self.target() {
            return Some(target);
        }
        if !self.is_forced() {
            return None;
        }
        self.controller_release.lock().unwrap().clone()
    }
}

/// How an update that did not re-exec left this host.
///
/// The two cases need different handling, so they are different values:
/// one is a host that is exactly where it started, the other is a host
/// whose binary and running process no longer agree.
pub enum UpdateFailure {
    /// Nothing was installed. The build on disk is still the build in
    /// this process, so trying again later starts from the same place.
    NotInstalled(anyhow::Error),
    /// The new build is installed but this process could not exec into
    /// it, so `pm --version` now reports one build while the running
    /// worker is still the old one and still speaks its protocol. Trying
    /// again only reinstalls a build this process will never run: the
    /// exec is the step that failed, and it fails the same way every
    /// time. Only a restart finishes it.
    InstalledNotRunning(anyhow::Error),
}

impl UpdateFailure {
    fn error(&self) -> &anyhow::Error {
        match self {
            UpdateFailure::NotInstalled(error) | UpdateFailure::InstalledNotRunning(error) => error,
        }
    }
}

/// Fetches `version`, verifies it, and replaces this process with it.
/// Only returns on failure, since a success re-execs.
pub async fn apply(version: &str) -> UpdateFailure {
    UpdateFailure::NotInstalled(match install(version).await {
        Ok(failure) => return failure,
        Err(error) => error,
    })
}

/// The part of an update that leaves this host as it found it. Returning
/// `Ok` means the install landed and only the exec is left, which is the
/// one step whose failure this process cannot recover from.
async fn install(version: &str) -> anyhow::Result<UpdateFailure> {
    if !update::updates_enabled() {
        return Err(anyhow::anyhow!(
            "this build cannot verify releases, so it will not update itself; install {version} by hand"
        ));
    }
    let client = reqwest::Client::builder()
        .user_agent(format!("pm/{}", pm_daemon::pm_build_version()))
        .build()
        .context("building the release client")?;
    let release = update::fetch_release(&client, update::Source::Exact(version))
        .await
        .context("reading the controller's release")?;
    if release.protocol_version != pm_protocol::WORKER_PROTOCOL_VERSION {
        info!(
            from = pm_protocol::WORKER_PROTOCOL_VERSION,
            to = release.protocol_version,
            "the controller's build speaks a different worker protocol, which is why this update matters"
        );
    }
    let binary = update::download_verified(&client, &release)
        .await
        .context("downloading the controller's release")?;
    let exe = update::install_over_current_exe(&binary)?;
    info!(version = %release.version, path = %exe.display(), "installed, restarting into the new build");
    Ok(UpdateFailure::InstalledNotRunning(update::reexec_current(
        &exe,
    )))
}

/// The build a just-registered worker should install before it serves
/// anything, if any. A worker already running agents is left to the idle
/// path, since restarting takes those agents down with it.
fn target_before_serving(pending: &PendingUpdate, live_terminals: usize) -> Option<String> {
    let target = pending.install_target()?;
    (live_terminals == 0).then_some(target)
}

/// Applies a pending update before this worker serves its controller.
///
/// A worker that has just booted holds no terminals, so restarting costs
/// nothing here, and this is the only dependable chance to do it: the
/// controller starts assigning work as soon as registration lands, and
/// once an agent is running the idle path waits for a lull that a busy
/// host may never get. Returns only if the update was skipped or failed,
/// in which case the worker serves on the build it has.
pub async fn apply_before_serving(pending: &PendingUpdate, mux: &pm_daemon::mux::Mux) {
    let live = mux.live_terminals().len();
    let Some(target) = target_before_serving(pending, live) else {
        if live > 0 {
            if let Some(target) = pending.install_target() {
                info!(
                    terminals = live,
                    target = %target,
                    "this worker is already serving agents, updating when it goes idle"
                );
            }
        }
        return;
    };
    info!(target = %target, "updating before serving, this worker holds no agents yet");
    report(apply(&target).await, &target, "before serving");
}

/// Logs a failed update as the two different situations it can be. The
/// distinction is the whole point: one host is still coherent and will
/// try again, the other has a binary its own process cannot run and
/// needs a hand.
fn report(failure: UpdateFailure, target: &str, when: &str) {
    let error = format!("{:#}", failure.error());
    match failure {
        UpdateFailure::NotInstalled(_) => error!(
            error = %error,
            target = %target,
            when,
            "worker update failed, staying on the current build"
        ),
        UpdateFailure::InstalledNotRunning(_) => error!(
            error = %error,
            target = %target,
            when,
            running = %update::release_version(pm_daemon::pm_build_version()),
            "worker update installed but could not restart into it, so this host is \
             running an older build than the one now on disk. Restart the pm worker \
             service to finish it"
        ),
    }
}

/// Applies a pending update as soon as the worker has no terminals left,
/// or straight away once an operator has forced it.
pub fn watch(pending: Arc<PendingUpdate>, mux: Arc<pm_daemon::mux::Mux>) {
    tokio::spawn(async move {
        let mut backoff = Backoff::default();
        loop {
            tokio::time::sleep(IDLE_POLL).await;
            let Some(target) = pending.install_target() else {
                continue;
            };
            let live = mux.live_terminals().len();
            if live > 0 && !pending.is_forced() {
                continue;
            }
            if backoff.holds_off(&target) {
                continue;
            }
            if live > 0 {
                warn!(
                    terminals = live,
                    "updating a busy worker on request. Its agents are killed and resumed \
                     afterwards with their history, but an in-flight turn does not continue"
                );
            }
            let failure = apply(&target).await;
            let installed = matches!(failure, UpdateFailure::InstalledNotRunning(_));
            report(
                failure,
                &target,
                if live > 0 { "on request" } else { "when idle" },
            );
            // Nothing else this process does can finish the update, so it
            // stops asking. Exiting to be restarted would finish it under a
            // unit that restarts, but `Restart=no` is a policy an operator
            // can choose, and turning a host that still answers into one
            // that is gone is the worse of the two failures.
            if installed {
                return;
            }
            backoff.record(target);
        }
    });
}

/// Spacing between failed update attempts.
///
/// A worker that cannot update usually cannot update for a reason that
/// outlasts one poll — a directory it may not write, a release that will
/// not verify — so retrying at the poll interval buys nothing and fills
/// the journal with the same error. Each failure doubles the wait up to
/// [`RETRY_MAX_POLLS`]. A different target is a different problem, so it
/// starts over.
#[derive(Default)]
struct Backoff {
    target: Option<String>,
    wait: u32,
    remaining: u32,
}

/// Polls skipped after a first failure, doubling from there.
const RETRY_MIN_POLLS: u32 = 1;
/// Ceiling on the wait, in polls. At a 20 second poll this is an attempt
/// roughly every ten minutes, which is often enough that a host recovers
/// on its own once the cause is fixed.
const RETRY_MAX_POLLS: u32 = 32;

impl Backoff {
    /// Whether this poll should pass without attempting `target`.
    fn holds_off(&mut self, target: &str) -> bool {
        if self.target.as_deref() != Some(target) {
            *self = Self::default();
            return false;
        }
        self.remaining = self.remaining.saturating_sub(1);
        self.remaining > 0
    }

    fn record(&mut self, target: String) {
        self.wait = if self.target.as_deref() == Some(target.as_str()) {
            (self.wait * 2).clamp(RETRY_MIN_POLLS, RETRY_MAX_POLLS)
        } else {
            RETRY_MIN_POLLS
        };
        self.remaining = self.wait + 1;
        self.target = Some(target);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn current() -> String {
        pm_daemon::pm_build_version().to_string()
    }

    #[test]
    fn a_controller_on_the_same_release_is_not_an_update() {
        let pending = PendingUpdate::default();
        pending.note_controller_build(&current());
        assert_eq!(pending.target(), None);
    }

    /// Only the release part is compared, so two builds of the same
    /// release from different commits never update each other.
    #[test]
    fn a_different_commit_of_the_same_release_is_not_an_update() {
        let pending = PendingUpdate::default();
        let same_release = format!("{}+deadbee", update::release_version(&current()));
        pending.note_controller_build(&same_release);
        assert_eq!(pending.target(), None);
    }

    #[test]
    fn a_different_release_becomes_the_target() {
        let pending = PendingUpdate::default();
        pending.note_controller_build("99.9.9+abc1234");
        assert_eq!(pending.target().as_deref(), Some("99.9.9"));
    }

    /// A controller on a channel build is followed by its exact version,
    /// suffix included, and followed back off it the same way.
    #[test]
    fn a_pre_release_controller_is_followed_by_its_exact_version() {
        let pending = PendingUpdate::default();
        pending.note_controller_build("0.10.0-dev.3+abc1234");
        assert_eq!(pending.target().as_deref(), Some("0.10.0-dev.3"));
        pending.note_controller_build("0.9.17+abc1234");
        assert_eq!(pending.target().as_deref(), Some("0.9.17"));
    }

    /// The controller going backwards is still a target: both ends have to
    /// speak the same worker protocol, so following it is the way back.
    #[test]
    fn a_rolled_back_controller_is_still_a_target() {
        let pending = PendingUpdate::default();
        pending.note_controller_build("0.0.1+abc1234");
        assert_eq!(pending.target().as_deref(), Some("0.0.1"));
    }

    /// An empty version reads as "no update", not as a target.
    #[test]
    fn a_controller_that_reports_no_version_is_not_a_target() {
        let pending = PendingUpdate::default();
        pending.note_controller_build("99.9.9");
        assert!(pending.target().is_some());
        pending.note_controller_build("");
        assert_eq!(pending.target(), None);
    }

    /// A controller that reports no version leaves nothing to force onto.
    #[test]
    fn forcing_against_a_controller_without_a_version_installs_nothing() {
        let pending = PendingUpdate::default();
        pending.note_controller_build("");
        pending.force();
        assert_eq!(pending.install_target(), None);
    }

    /// The recovery path a protocol refusal takes. The controller names its
    /// build when it rejects a worker, and reinstalling that release is the
    /// only way back. Waiting for idle cannot help, because the release
    /// matches and nothing is pending, so only a forced update recovers.
    #[test]
    fn a_forced_update_recovers_a_protocol_refusal_from_the_same_release() {
        let pending = PendingUpdate::default();
        let release = update::release_version(&current()).to_string();
        pending.note_controller_build(&format!("{release}+deadbee"));

        assert_eq!(pending.target(), None, "nothing is pending on its own");
        assert_eq!(
            pending.install_target(),
            None,
            "and an unforced worker leaves the matching release alone"
        );

        pending.force();
        assert_eq!(
            pending.install_target().as_deref(),
            Some(release.as_str()),
            "the operator's force installs the controller's release"
        );
    }

    /// Forcing with nothing pending used to do nothing at all: the watcher
    /// required a target before it ever consulted the flag, so the operator
    /// got silence. The controller's release is what force means.
    #[test]
    fn forcing_installs_the_controller_release_when_nothing_is_pending() {
        let pending = PendingUpdate::default();
        let same_release = format!("{}+deadbee", update::release_version(&current()));
        pending.note_controller_build(&same_release);
        assert_eq!(pending.install_target(), None, "idle hosts stay put");

        pending.force();
        assert_eq!(
            pending.install_target().as_deref(),
            Some(update::release_version(&current())),
        );
    }

    /// Attempts, expressed as the polls that actually tried. A worker
    /// that cannot update usually still cannot a poll later, and the old
    /// loop retried every poll forever, re-logging the same warning.
    fn attempts(polls: usize, target: &str) -> Vec<usize> {
        let mut backoff = Backoff::default();
        (0..polls)
            .filter(|_| {
                let tried = !backoff.holds_off(target);
                if tried {
                    backoff.record(target.to_string());
                }
                tried
            })
            .collect()
    }

    #[test]
    fn a_failing_update_backs_off_instead_of_retrying_every_poll() {
        assert_eq!(attempts(16, "9.9.9"), vec![0, 2, 5, 10]);
    }

    /// The wait stops growing, so a host still recovers on its own once
    /// whatever blocked it is fixed.
    #[test]
    fn the_backoff_stops_growing_at_the_ceiling() {
        let mut backoff = Backoff::default();
        for _ in 0..20 {
            backoff.record("9.9.9".to_string());
        }
        assert_eq!(backoff.wait, RETRY_MAX_POLLS);
    }

    /// A new target is a different problem, so it is tried at once rather
    /// than serving out the previous target's wait.
    #[test]
    fn a_new_target_is_attempted_without_waiting() {
        let mut backoff = Backoff::default();
        backoff.record("9.9.9".to_string());
        backoff.record("9.9.9".to_string());
        assert!(
            backoff.holds_off("9.9.9"),
            "the old target is still waiting"
        );
        assert!(!backoff.holds_off("1.1.1"), "a new target waits on nothing");
    }

    /// Force still needs to have heard from a controller. Nothing to install
    /// is not the same as installing nothing in particular.
    #[test]
    fn forcing_before_any_controller_is_known_installs_nothing() {
        let pending = PendingUpdate::default();
        pending.force();
        assert_eq!(pending.install_target(), None);
    }

    /// A worker that has just booted holds nothing, so the update happens
    /// there rather than waiting on an idle moment it may never get.
    #[test]
    fn a_freshly_booted_worker_updates_before_it_serves() {
        let pending = PendingUpdate::default();
        pending.note_controller_build("99.9.9+abc1234");
        assert_eq!(
            target_before_serving(&pending, 0).as_deref(),
            Some("99.9.9")
        );
    }

    /// Restarting takes running agents down with it, so a worker that is
    /// already serving is left to the idle path.
    #[test]
    fn a_worker_already_serving_agents_does_not_update_before_serving() {
        let pending = PendingUpdate::default();
        pending.note_controller_build("99.9.9+abc1234");
        assert_eq!(target_before_serving(&pending, 1), None);
    }

    /// Matching builds are not an update, however idle the worker is.
    #[test]
    fn a_worker_on_the_controllers_build_updates_nothing_before_serving() {
        let pending = PendingUpdate::default();
        pending.note_controller_build(&current());
        assert_eq!(target_before_serving(&pending, 0), None);
    }

    /// A real cross-release target outranks the forced fallback.
    #[test]
    fn a_pending_target_wins_over_the_forced_fallback() {
        let pending = PendingUpdate::default();
        pending.note_controller_build("99.9.9+abc1234");
        pending.force();
        assert_eq!(pending.install_target().as_deref(), Some("99.9.9"));
    }

    /// The force flag is never cleared, so a force that could not run
    /// still applies to whatever target arrives later.
    #[test]
    fn a_force_outlives_the_poll_that_could_not_act_on_it() {
        let pending = PendingUpdate::default();
        pending.force();
        pending.note_controller_build("99.9.9+abc1234");
        assert!(pending.is_forced());
        assert_eq!(pending.target().as_deref(), Some("99.9.9"));
    }
}
