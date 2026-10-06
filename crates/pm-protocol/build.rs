use std::path::PathBuf;

fn main() {
    // Read at build-script runtime, not compile time: a cached build
    // script must resolve the current worktree, not the one it was
    // compiled in (target dirs are shared across worktrees).
    let manifest_dir = std::env::var("CARGO_MANIFEST_DIR").expect("CARGO_MANIFEST_DIR");
    let proto_root = PathBuf::from(manifest_dir).join("../../proto");
    let proto_file = proto_root.join("pm/v1/pm.proto");
    println!("cargo:rerun-if-changed={}", proto_file.display());

    let descriptors = protox::compile([&proto_file], [&proto_root]).expect("compile proto");
    prost_build::Config::new()
        // Keeps the resolved endpoint from dominating the size of every
        // ControllerMessage.
        .boxed(".pm.v1.ControllerSpawn.model_endpoint")
        .compile_fds(descriptors)
        .expect("generate rust from proto");
}
