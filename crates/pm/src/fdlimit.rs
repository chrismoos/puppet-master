//! Raises this process's open-file limit at startup.
//!
//! A daemon or worker holds three descriptors per terminal — the pty
//! master plus the reader and writer dups — and two more for every
//! forwarded connection, one to the target and one dialled back to the
//! controller. A host with a few dozen sessions is already close to the
//! 1024-descriptor soft limit most distributions ship, and a browser
//! opening a published page in parallel pushes it over. The hard limit
//! is far higher, so the soft one is raised to meet it.

use tracing::{debug, info, warn};

/// The soft limit to ask for. A host whose hard limit is lower gets that
/// instead.
const WANTED_OPEN_FILES: libc::rlim_t = 65_536;

/// Raises the soft open-file limit toward [`WANTED_OPEN_FILES`]. A
/// failure is logged and nothing else: the process runs with whatever
/// the platform allows rather than refusing to start.
pub fn raise_open_files() {
    let mut limit = libc::rlimit {
        rlim_cur: 0,
        rlim_max: 0,
    };
    if unsafe { libc::getrlimit(libc::RLIMIT_NOFILE, &mut limit) } != 0 {
        debug!(
            error = %std::io::Error::last_os_error(),
            "could not read the open-file limit"
        );
        return;
    }
    let Some(wanted) = wanted_soft_limit(limit.rlim_cur, limit.rlim_max) else {
        debug!(
            open_files = limit.rlim_cur,
            "the open-file limit is already high enough"
        );
        return;
    };
    let raised = libc::rlimit {
        rlim_cur: wanted,
        rlim_max: limit.rlim_max,
    };
    if unsafe { libc::setrlimit(libc::RLIMIT_NOFILE, &raised) } != 0 {
        warn!(
            from = limit.rlim_cur,
            to = wanted,
            error = %std::io::Error::last_os_error(),
            "could not raise the open-file limit; sessions and forwards may exhaust it"
        );
        return;
    }
    info!(
        from = limit.rlim_cur,
        to = wanted,
        "raised the open-file limit"
    );
}

/// The soft limit to ask for, or None when the current one already
/// covers what this process wants.
fn wanted_soft_limit(current: libc::rlim_t, hard: libc::rlim_t) -> Option<libc::rlim_t> {
    let target = hard.min(WANTED_OPEN_FILES);
    (target > current).then_some(target)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_distribution_default_is_raised_to_what_this_process_wants() {
        assert_eq!(wanted_soft_limit(1024, 1_048_576), Some(WANTED_OPEN_FILES));
    }

    #[test]
    fn the_hard_limit_caps_the_request() {
        assert_eq!(wanted_soft_limit(1024, 4096), Some(4096));
    }

    /// Asking for less than the process already has would lower it.
    #[test]
    fn a_limit_that_is_already_high_enough_is_left_alone() {
        assert_eq!(wanted_soft_limit(WANTED_OPEN_FILES, 1_048_576), None);
        assert_eq!(wanted_soft_limit(1_048_576, 1_048_576), None);
        assert_eq!(wanted_soft_limit(4096, 4096), None);
    }

    fn current_limit() -> libc::rlimit {
        let mut limit = libc::rlimit {
            rlim_cur: 0,
            rlim_max: 0,
        };
        assert_eq!(
            unsafe { libc::getrlimit(libc::RLIMIT_NOFILE, &mut limit) },
            0
        );
        limit
    }

    /// A raise is only observable from below, and a test host may
    /// already sit above what this process asks for, so the soft limit
    /// is lowered first and put back before anything is asserted.
    #[test]
    fn raising_moves_the_real_limit_on_this_platform() {
        let original = current_limit();
        let target = original.rlim_max.min(WANTED_OPEN_FILES);
        let lowered = libc::rlimit {
            rlim_cur: target / 2,
            rlim_max: original.rlim_max,
        };
        assert_eq!(unsafe { libc::setrlimit(libc::RLIMIT_NOFILE, &lowered) }, 0);

        raise_open_files();
        let after = current_limit();
        assert_eq!(
            unsafe { libc::setrlimit(libc::RLIMIT_NOFILE, &original) },
            0
        );

        assert_eq!(after.rlim_cur, target);
    }
}
