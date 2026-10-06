fn main() {
    emit_git_revision();

    // rust-embed requires the UI dist folder to exist at compile time,
    // even for debug builds that read it from disk at runtime.
    let manifest_dir = std::env::var("CARGO_MANIFEST_DIR").expect("CARGO_MANIFEST_DIR");
    let dist = std::path::Path::new(&manifest_dir).join("../../web/dist");
    std::fs::create_dir_all(&dist).expect("create web/dist placeholder");

    if std::env::var("PROFILE").as_deref() == Ok("release") {
        println!("cargo:rerun-if-changed={}", dist.display());
        emit_rerun_recursive(&dist);
    }
}

/// Captures the supplied build revision, or the local short git revision
/// (with a -dirty suffix) when no override is supplied. Falls back to
/// "unknown" when neither source is available.
fn emit_git_revision() {
    println!("cargo:rerun-if-env-changed=PM_GIT_REV");
    let override_rev = std::env::var("PM_GIT_REV")
        .ok()
        .filter(|rev| !rev.trim().is_empty());
    let rev = match override_rev {
        Some(rev) => rev,
        None => git_revision().unwrap_or_else(|| "unknown".to_string()),
    };
    println!("cargo:rustc-env=PM_GIT_REV={rev}");
    if let Some(head) = git_path("HEAD") {
        println!("cargo:rerun-if-changed={head}");
    }
}

fn git_revision() -> Option<String> {
    let rev = std::process::Command::new("git")
        .args(["rev-parse", "--short", "HEAD"])
        .output()
        .ok()
        .filter(|o| o.status.success())
        .map(|o| String::from_utf8_lossy(&o.stdout).trim().to_string())?;
    if rev.is_empty() {
        return None;
    }
    let dirty = std::process::Command::new("git")
        .args(["status", "--porcelain"])
        .output()
        .map(|o| !o.stdout.is_empty())
        .unwrap_or(false);
    Some(if dirty { format!("{rev}-dirty") } else { rev })
}

fn git_path(name: &str) -> Option<String> {
    std::process::Command::new("git")
        .args(["rev-parse", "--git-path", name])
        .output()
        .ok()
        .filter(|output| output.status.success())
        .map(|output| String::from_utf8_lossy(&output.stdout).trim().to_string())
        .filter(|path| !path.is_empty())
}

fn emit_rerun_recursive(dir: &std::path::Path) {
    let Ok(entries) = std::fs::read_dir(dir) else {
        return;
    };
    for entry in entries.flatten() {
        let path = entry.path();
        println!("cargo:rerun-if-changed={}", path.display());
        if path.is_dir() {
            emit_rerun_recursive(&path);
        }
    }
}
