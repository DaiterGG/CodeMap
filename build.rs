use std::env;
use std::fs;
use std::io;
use std::path::PathBuf;
use std::process::Command;

#[path = "src/page_template.rs"]
mod page_template;

const WASM_TARGET: &str = "wasm32-unknown-unknown";
const WASM_PACKAGE: &str = "codemap-browser-core";
const WASM_FILE: &str = "codemap_browser_core.wasm";
const EMPTY_GRAPH_JSON: &str = r#"{"direction":"TD","name":null,"description":null,"width":320.0,"height":200.0,"nodes":[],"edges":[]}"#;

fn main() {
    println!("cargo:rerun-if-changed=src/page_template.rs");
    println!("cargo:rerun-if-changed=public/index.html");
    println!("cargo:rerun-if-changed=public/vendor/elk.bundled.js");
    println!("cargo:rerun-if-changed=public/vendor/elk.LICENSE");
    println!("cargo:rerun-if-changed=public/vendor/elkjs-libavoid.bundled.js");
    println!("cargo:rerun-if-changed=public/vendor/libavoid.wasm");
    println!("cargo:rerun-if-changed=public/vendor/elkjs-libavoid.LICENSE");
    println!("cargo:rerun-if-changed=public/vendor/libavoid-js.LICENSE");
    println!("cargo:rerun-if-changed=public/vendor/highlight.min.js");
    println!("cargo:rerun-if-changed=prompts/nodegraph.txt");
    println!("cargo:rerun-if-changed=prompts/tools/common/system.txt");
    println!("cargo:rerun-if-changed=prompts/tools/common/error-recovery.txt");
    println!("cargo:rerun-if-changed=prompts/tools/common/errors/invalid-arguments.txt");
    println!("cargo:rerun-if-changed=prompts/tools/common/errors/invalid-command.txt");
    println!("cargo:rerun-if-changed=prompts/tools/common/errors/unclosed-quote.txt");
    println!("cargo:rerun-if-changed=prompts/tools/rg/system.txt");
    println!("cargo:rerun-if-changed=prompts/tools/rg/help.txt");
    println!("cargo:rerun-if-changed=prompts/tools/rg/errors/invalid-glob.txt");
    println!("cargo:rerun-if-changed=prompts/tools/rg/errors/invalid-options.txt");
    println!("cargo:rerun-if-changed=prompts/tools/rg/errors/invalid-pattern.txt");
    println!("cargo:rerun-if-changed=prompts/tools/rg/errors/missing-pattern.txt");
    println!("cargo:rerun-if-changed=prompts/tools/rg/errors/no-loaded-files.txt");
    println!("cargo:rerun-if-changed=prompts/tools/rg/errors/no-matches.txt");
    println!("cargo:rerun-if-changed=prompts/tools/rg/errors/path-not-loaded.txt");
    println!("cargo:rerun-if-changed=prompts/tools/rg/errors/unsafe-path.txt");
    println!("cargo:rerun-if-changed=prompts/tools/rg/errors/unsupported-option.txt");
    println!("cargo:rerun-if-changed=prompts/tools/cat/system.txt");
    println!("cargo:rerun-if-changed=prompts/tools/cat/help.txt");
    println!("cargo:rerun-if-changed=prompts/tools/cat/errors/missing-path.txt");
    println!("cargo:rerun-if-changed=prompts/tools/cat/errors/path-not-loaded.txt");
    println!("cargo:rerun-if-changed=prompts/tools/cat/errors/standard-input.txt");
    println!("cargo:rerun-if-changed=prompts/tools/cat/errors/unsafe-path.txt");
    println!("cargo:rerun-if-changed=prompts/tools/cat/errors/unsupported-option.txt");
    println!("cargo:rerun-if-changed=prompts/tools/sed/system.txt");
    println!("cargo:rerun-if-changed=prompts/tools/sed/help.txt");
    println!("cargo:rerun-if-changed=prompts/tools/sed/errors/in-place-edit.txt");
    println!("cargo:rerun-if-changed=prompts/tools/sed/errors/invalid-regex.txt");
    println!("cargo:rerun-if-changed=prompts/tools/sed/errors/invalid-script.txt");
    println!("cargo:rerun-if-changed=prompts/tools/sed/errors/missing-path.txt");
    println!("cargo:rerun-if-changed=prompts/tools/sed/errors/missing-script.txt");
    println!("cargo:rerun-if-changed=prompts/tools/sed/errors/path-not-loaded.txt");
    println!("cargo:rerun-if-changed=prompts/tools/sed/errors/unsafe-path.txt");
    println!("cargo:rerun-if-changed=prompts/tools/sed/errors/standard-input.txt");
    println!("cargo:rerun-if-changed=prompts/tools/sed/errors/unsupported-command.txt");
    println!("cargo:rerun-if-changed=prompts/tools/sed/errors/unsupported-option.txt");
    println!("cargo:rerun-if-changed=browser-core/Cargo.toml");
    println!("cargo:rerun-if-changed=browser-core/src/lib.rs");
    println!("cargo:rerun-if-changed=browser-core/src/rg.rs");
    println!("cargo:rerun-if-changed=browser-core/src/cat.rs");
    println!("cargo:rerun-if-changed=browser-core/src/sed.rs");
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
    println!("cargo:rerun-if-changed=.env");
    println!("cargo:rerun-if-changed=../.env");
    println!("cargo:rerun-if-changed=Cargo.lock");
    println!("cargo:rerun-if-env-changed=OPEN_ROUTER_KEY");
    println!("cargo:rerun-if-env-changed=OPEN_ROUTER");

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

    publish_page(&out_dir).unwrap_or_else(|error| {
        panic!("could not assemble the static page in ./index.html: {error}")
    });
}

fn publish_page(out_dir: &std::path::Path) -> io::Result<()> {
    let api_key = configured_api_key()?;
    let wasm = fs::read(out_dir.join("codemap-core.wasm"))?;
    let bindings = fs::read_to_string(out_dir.join("codemap_core.js"))?;
    let html = page_template::render_page_html(
        "",
        EMPTY_GRAPH_JSON,
        "{}",
        &wasm,
        &bindings,
        include_str!("prompts/nodegraph.txt"),
        &api_key,
    );
    let index_path =
        PathBuf::from(env::var_os("CARGO_MANIFEST_DIR").expect("Cargo sets CARGO_MANIFEST_DIR"))
            .join("index.html");
    fs::write(index_path, html)
}

fn configured_api_key() -> io::Result<String> {
    let key_names = ["OPEN_ROUTER_KEY", "OPEN_ROUTER"];
    if let Some(key) = key_names
        .iter()
        .find_map(|name| env::var(name).ok().filter(|key| !key.trim().is_empty()))
    {
        return Ok(key);
    }

    let manifest_dir =
        PathBuf::from(env::var_os("CARGO_MANIFEST_DIR").expect("Cargo sets CARGO_MANIFEST_DIR"));
    let root_env_path = manifest_dir.join(".env");
    let parent_env_path = manifest_dir.parent().map(|parent| parent.join(".env"));
    let env_path = if root_env_path.is_file() {
        Some(root_env_path)
    } else {
        parent_env_path.filter(|path| path.is_file())
    };

    if let Some(path) = env_path {
        dotenvy::from_path(&path).map_err(|error| {
            io::Error::other(format!(
                "could not load build-time environment from {}: {error}",
                path.display()
            ))
        })?;
    }

    Ok(key_names
        .iter()
        .find_map(|name| env::var(name).ok().filter(|key| !key.trim().is_empty()))
        .unwrap_or_default())
}
