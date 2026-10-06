//! The log filter `pm daemon`, `pm worker` and `pm pushgw` start with.
//!
//! Asking for detail should buy pm's own detail. `RUST_LOG=trace` buys
//! the transport dependencies' detail instead — mio, tungstenite and
//! rustls out-log pm's own code by orders of magnitude at that level —
//! so the verbosity flag raises pm's crates and leaves everything else
//! where it was. `RUST_LOG` still overrides the whole thing for anyone
//! who wants a specific target or the raw firehose.

use tracing_subscriber::EnvFilter;

/// The crates whose targets a verbosity flag raises. Everything pm logs
/// comes from one of these, and nothing else does.
const PM_CRATES: [&str; 9] = [
    "pm",
    "pm_daemon",
    "pm_pushgw",
    "pm_adapters",
    "pm_client",
    "pm_protocol",
    "pm_tls",
    "pm_push",
    "pm_tui",
];

/// Dependencies that carry the socket and dwarf pm's own output once
/// they are turned up. They are held down so that raising pm does not
/// raise them with it.
const TRANSPORT_CRATES: [&str; 12] = [
    "mio",
    "tungstenite",
    "tokio_tungstenite",
    "tokio_util",
    "rustls",
    "tokio_rustls",
    "hyper",
    "hyper_util",
    "h2",
    "axum",
    "tower",
    "want",
];

/// The level pm's crates log at for each `-v`. Past the end of this, the
/// filter stops scoping and everything logs at trace.
const PM_LEVELS: [&str; 3] = ["info", "debug", "trace"];

/// Installs the process-wide log filter. `RUST_LOG` wins when it is set,
/// so an operator who knows the target they want keeps saying so.
pub fn init(verbosity: u8) {
    let from_env = std::env::var("RUST_LOG")
        .ok()
        .filter(|value| !value.trim().is_empty());
    let filter = match &from_env {
        Some(_) => EnvFilter::try_from_default_env().unwrap_or_else(|_| EnvFilter::new("info")),
        None => EnvFilter::new(directives(verbosity)),
    };
    tracing_subscriber::fmt().with_env_filter(filter).init();
    if from_env.is_some() && verbosity > 0 {
        tracing::warn!("RUST_LOG is set, so --verbose does not apply to this run");
    }
}

/// The filter one `-v` count means, as a directive string.
fn directives(verbosity: u8) -> String {
    let Some(level) = PM_LEVELS.get(verbosity as usize) else {
        return "trace".to_string();
    };
    if verbosity == 0 {
        return "info".to_string();
    }
    let mut directives = vec!["info".to_string()];
    directives.extend(TRANSPORT_CRATES.iter().map(|krate| format!("{krate}=warn")));
    directives.extend(PM_CRATES.iter().map(|krate| format!("{krate}={level}")));
    directives.join(",")
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A run with no flag has to keep logging exactly what it logged
    /// before the flag existed, or turning it on becomes the only way
    /// back to a working default.
    #[test]
    fn no_flag_is_plain_info() {
        assert_eq!(directives(0), "info");
    }

    /// The failure the flag exists for: detail that is pm's detail, not
    /// the socket library's.
    #[test]
    fn asking_for_detail_raises_pm_and_holds_the_transport_down() {
        let one = directives(1);
        assert!(one.contains("pm=debug"), "{one}");
        assert!(one.contains("pm_daemon=debug"), "{one}");
        assert!(one.contains("mio=warn"), "{one}");
        assert!(one.contains("tungstenite=warn"), "{one}");
        assert!(!one.contains("mio=debug"), "{one}");

        let two = directives(2);
        assert!(two.contains("pm=trace"), "{two}");
        assert!(two.contains("pm_daemon=trace"), "{two}");
        assert!(two.contains("mio=warn"), "{two}");
        assert!(!two.contains("mio=trace"), "{two}");
    }

    /// Past the scoped levels the flag stops scoping, so the firehose is
    /// still reachable without knowing a crate name.
    #[test]
    fn the_last_step_is_the_unscoped_firehose() {
        assert_eq!(directives(3), "trace");
        assert_eq!(directives(9), "trace");
    }

    /// Every directive the flag builds has to parse, or the process
    /// starts with no filter at all.
    #[test]
    fn every_level_parses_as_a_filter() {
        for verbosity in 0..=4 {
            let built = directives(verbosity);
            built
                .parse::<EnvFilter>()
                .unwrap_or_else(|e| panic!("verbosity {verbosity} built {built:?}: {e}"));
        }
    }
}
