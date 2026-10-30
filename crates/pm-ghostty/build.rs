use sha2::{Digest, Sha256};
use std::{env, fs, path::PathBuf};

fn main() {
    let root = PathBuf::from(env::var_os("CARGO_MANIFEST_DIR").unwrap());
    let vendor = root.join("vendor");
    let target = env::var("TARGET").unwrap();
    let manifest =
        fs::read_to_string(vendor.join("SHA256SUMS")).expect("missing Ghostty artifact manifest");
    println!("cargo:rerun-if-changed=src/bridge.c");
    println!("cargo:rerun-if-changed=vendor");
    for name in [
        "src/bridge.c".to_owned(),
        format!("vendor/{target}/libghostty-vt-static.a"),
        format!("vendor/{target}/libpm-ghostty-bridge.a"),
    ] {
        let expected = manifest
            .lines()
            .find_map(|entry| {
                let (hash, path) = entry.split_once("  ")?;
                (path == name).then_some(hash)
            })
            .unwrap_or_else(|| panic!("unsupported or unverified Ghostty artifact {name}"));
        let bytes = fs::read(root.join(&name))
            .unwrap_or_else(|_| panic!("missing Ghostty artifact {name}"));
        assert_eq!(
            format!("{:x}", Sha256::digest(bytes)),
            expected,
            "Ghostty artifact {name} changed: rebuild the pinned runtime"
        );
    }
    let lib = vendor.join(&target);
    assert!(
        lib.join("libghostty-vt-static.a").is_file(),
        "unsupported Ghostty target {target}"
    );
    println!("cargo:rustc-link-search=native={}", lib.display());
    println!("cargo:rustc-link-lib=static=pm-ghostty-bridge");
    println!("cargo:rustc-link-lib=static=ghostty-vt-static");
}
