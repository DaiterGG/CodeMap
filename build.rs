use std::env;
use std::fs;
use std::path::Path;
use std::process::Command;

fn main() {
    let manifest_dir_value = env::var("CARGO_MANIFEST_DIR").expect("CARGO_MANIFEST_DIR is not set");
    let manifest_dir = Path::new(&manifest_dir_value);
    let router_dir = manifest_dir.join("wasm-router");

    println!("cargo:rerun-if-changed=wasm-router/Cargo.toml");
    println!("cargo:rerun-if-changed=wasm-router/Cargo.lock");
    println!("cargo:rerun-if-changed=wasm-router/src");

    let status = Command::new("cargo")
        .args([
            "build",
            "--manifest-path",
            router_dir.join("Cargo.toml").to_string_lossy().as_ref(),
            "--target",
            "wasm32-unknown-unknown",
            "--release",
        ])
        .current_dir(manifest_dir)
        .status()
        .expect("could not start the libavoid WASM build");
    assert!(
        status.success(),
        "libavoid WASM build failed; install the wasm32-unknown-unknown Rust target"
    );

    let wasm_path = router_dir
        .join("target")
        .join("wasm32-unknown-unknown")
        .join("release")
        .join("codemap_router.wasm");
    let output_path =
        Path::new(&env::var("OUT_DIR").expect("OUT_DIR is not set")).join("codemap_router.wasm");
    fs::copy(wasm_path, output_path).expect("could not copy the built libavoid WASM module");
}
