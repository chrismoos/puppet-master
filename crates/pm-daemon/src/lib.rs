#![recursion_limit = "256"]
pub mod appearance;
pub mod attachments;
pub mod auth;
pub mod connection;
pub mod connections;
pub mod daemon;
pub mod dir_server;
pub mod forward;
pub mod forward_mount;
pub(crate) mod forward_proxy;
mod forward_route;
pub mod forward_upstream;
pub mod fsperm;
pub mod harness_install;
pub mod hostname;
pub mod http;
pub mod inbox;
pub mod mcp;
pub mod mobile;
pub mod mux;
pub mod plan;
pub mod plan_store;
pub mod probe_trace;
pub mod project_host;
pub mod push;
pub(crate) mod report_freshness;
pub mod review;
pub mod review_diff;
pub mod review_repo;
pub mod review_store;
pub mod secrets;
pub mod server;
pub mod stale_turn;
pub mod storage;
pub mod supervisor_wake;
pub mod term_model;
pub mod terminal_theme;
pub mod text;
pub mod ui_theme;
pub mod update;
pub mod web_origin;
pub mod worker_dialer;
pub mod worker_plane;
pub mod workers;

use std::sync::Arc;

pub use daemon::{Daemon, DaemonConfig, HttpTls};
pub use server::ServerHandle;

/// The build identity of this pm binary: crate version plus the git
/// revision it was built from. Workers report it at registration so the
/// Hosts page can show binary drift against the controller.
pub fn pm_build_version() -> &'static str {
    concat!(env!("CARGO_PKG_VERSION"), "+", env!("PM_GIT_REV"))
}

/// How often the daemon asks what the newest published build is.
const RELEASE_CHECK_INTERVAL: std::time::Duration = std::time::Duration::from_secs(6 * 60 * 60);

/// Builds the daemon and starts serving on the configured socket and,
/// when configured, the HTTP surface.
pub async fn start(config: DaemonConfig) -> anyhow::Result<(Arc<Daemon>, ServerHandle)> {
    let socket_path = config.socket_path.clone();
    let http_addr = config.http_addr;
    let http_tls = config.http_tls.clone();
    let worker_addr = config.worker_addr;
    let (daemon, channels) = Daemon::new(config)?;
    let daemon = Arc::new(daemon);
    let handle = server::start(
        daemon.clone(),
        socket_path,
        http_addr,
        http_tls,
        worker_addr,
        channels,
    )
    .await?;
    // Started here rather than in Daemon::new, which has to stay callable
    // without a reactor: plenty of tests construct a daemon synchronously.
    crate::update::watch_latest(
        daemon.latest_release.clone(),
        daemon.release_channel().map(str::to_string),
        RELEASE_CHECK_INTERVAL,
    );
    daemon.recover_local_terminals();
    daemon.resume_connection_calls()?;
    tokio::spawn(forward::run_reconciler(daemon.clone()));
    tokio::spawn(worker_dialer::run(daemon.clone()));
    Ok((daemon, handle))
}

mod viewer_ownership;
