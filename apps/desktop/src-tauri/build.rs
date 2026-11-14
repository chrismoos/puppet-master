include!("src/commands.rs");

fn main() {
    println!("cargo:rerun-if-changed=src/commands.rs");
    tauri_build::try_build(
        tauri_build::Attributes::new()
            .app_manifest(tauri_build::AppManifest::new().commands(COMMANDS)),
    )
    .expect("the Tauri build step succeeds");
}
