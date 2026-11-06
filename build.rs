use std::env;
use std::fs;
use std::path::PathBuf;
use std::process::Command;

const WASM_TARGET: &str = "wasm32-unknown-unknown";
const WASM_PACKAGE: &str = "codemap-browser-core";
const WASM_FILE: &str = "codemap_browser_core.wasm";

fn main() {
    println!("cargo:rerun-if-changed=browser-core/Cargo.toml");
    println!("cargo:rerun-if-changed=browser-core/src/lib.rs");
    println!("cargo:rerun-if-changed=src/graph_parser.rs");
    println!("cargo:rerun-if-changed=src/multiagent.rs");
    println!("cargo:rerun-if-changed=prompts/nodegraph.txt");
    println!("cargo:rerun-if-changed=prompts/multiagent_start.txt");
    println!("cargo:rerun-if-changed=prompts/multiagent_sequential.txt");
    println!("cargo:rerun-if-changed=prompts/multiagent_parallel.txt");
    println!("cargo:rerun-if-changed=prompts/syntax_error.txt");
    println!("cargo:rerun-if-changed=prompts/redundant_connection.txt");
    println!("cargo:rerun-if-changed=prompts/source_reference_error.txt");
    println!("cargo:rerun-if-changed=prompts/partial_error.txt");
    println!("cargo:rerun-if-changed=Cargo.lock");

    let out_dir = PathBuf::from(env::var_os("OUT_DIR").expect("Cargo sets OUT_DIR"));
    let target_dir = out_dir.join("wasm-target");
    let cargo = env::var_os("CARGO").unwrap_or_else(|| "cargo".into());
    let status = Command::new(cargo)
        .current_dir(env::var_os("CARGO_MANIFEST_DIR").expect("Cargo sets CARGO_MANIFEST_DIR"))
        .args([
            "build",
            "--package",
            WASM_PACKAGE,
            "--target",
            WASM_TARGET,
            "--release",
            "--target-dir",
        ])
        .arg(&target_dir)
        .status()
        .unwrap_or_else(|error| panic!("could not run Cargo to build the browser core: {error}"));

    if !status.success() {
        panic!(
            "could not build the browser WebAssembly module; make sure the \
             `{WASM_TARGET}` target is installed with `rustup target add {WASM_TARGET}`"
        );
    }

    let wasm_path = target_dir.join(WASM_TARGET).join("release").join(WASM_FILE);
    let mut bindgen = wasm_bindgen_cli_support::Bindgen::new();
    bindgen.input_path(&wasm_path);
    bindgen.out_name("codemap_core");
    bindgen
        .no_modules(true)
        .expect("no-modules output is supported");
    bindgen
        .generate(&out_dir)
        .unwrap_or_else(|error| panic!("could not generate browser WASM bindings: {error}"));

    let generated_wasm_path = out_dir.join("codemap_core_bg.wasm");
    let embedded_wasm_path = out_dir.join("codemap-core.wasm");
    let wasm_source = if generated_wasm_path.is_file() {
        generated_wasm_path
    } else {
        wasm_path
    };
    fs::copy(&wasm_source, embedded_wasm_path).unwrap_or_else(|error| {
        panic!(
            "could not copy browser WebAssembly module from {}: {error}",
            wasm_source.display()
        )
    });
}
