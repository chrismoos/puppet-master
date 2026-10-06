//! The machine's own hostname. Both the worker and the forward URLs fall
//! back to it when no name has been configured, so it has to answer on
//! every platform a worker runs on, not just Linux.

/// The machine hostname, or `fallback` when the system reports nothing
/// usable.
pub fn machine_hostname(fallback: &str) -> String {
    normalize(read_hostname(), fallback)
}

fn normalize(raw: Option<String>, fallback: &str) -> String {
    raw.map(|name| name.trim().to_string())
        .filter(|name| !name.is_empty())
        .unwrap_or_else(|| fallback.to_string())
}

#[cfg(target_os = "linux")]
fn read_hostname() -> Option<String> {
    std::fs::read_to_string("/proc/sys/kernel/hostname").ok()
}

#[cfg(all(unix, not(target_os = "linux")))]
fn read_hostname() -> Option<String> {
    let output = std::process::Command::new("hostname").output().ok()?;
    String::from_utf8(output.stdout).ok()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_reported_name_is_trimmed_and_blank_ones_fall_back() {
        assert_eq!(normalize(Some("  box-01\n".into()), "worker"), "box-01");
        assert_eq!(normalize(Some("   \n".into()), "worker"), "worker");
        assert_eq!(normalize(Some(String::new()), "localhost"), "localhost");
        assert_eq!(normalize(None, "localhost"), "localhost");
    }

    /// Every supported platform has to answer, since a fallback name makes
    /// two hosts indistinguishable in the UI.
    #[test]
    fn this_platform_reports_its_own_name() {
        assert_ne!(machine_hostname("fallback-used"), "fallback-used");
    }
}
