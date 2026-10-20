//! CLI orchestration: collect source files, request a custom graph from the LLM,
//! validate and retry its output, then export the final graph as HTML and files.
//! `--gr FILE` skips collection and LLM work and renders the supplied graph file.

use std::env;
use std::path::PathBuf;
use std::process;

use hello_cli::llm::OpenRouter;
use hello_cli::step::{Message, StepStack};

const DEFAULT_MODEL: &str = "liquid/lfm-2.5-2.6b:free";
const DEFAULT_PATH: &str = "examples/small_repo";
const MAX_RETRIES: usize = 3;
const AVAILABLE_MODELS: &[&str] = &[
    "liquid/lfm-2.5-2.6b:free",
    "inception/mercury-decide:free",
    "xingchen-agi/xing4.0-29b-a4b:free",
];

const USAGE: &str = "Usage: hello-cli [OPTIONS]

Read files under PATH and ask the configured OpenRouter model a question.

Options:
      --path PATH  Directory to read (default: examples/small_repo)
      --gr FILE    Render a custom graph file as HTML and open it in a browser
      --no-readme  Exclude files named README (case-insensitive, any extension)
      --no-md      Exclude Markdown files (.md and .markdown)
      --no-browser  Write the response page without opening a browser
      --model ID    Select a model:
                      liquid/lfm-2.5-2.6b:free (default)
                      inception/mercury-decide:free
                      xingchen-agi/xing4.0-29b-a4b:free
  -h, --help       Print help
  -V, --version    Print version";

fn main() {
    let mut path = PathBuf::from(DEFAULT_PATH);
    let mut path_provided = false;
    let mut graph_path = None;
    let mut model = DEFAULT_MODEL;
    let mut open_browser = true;
    let mut exclude_readme = false;
    let mut exclude_markdown = false;
    let mut args = env::args().skip(1);
    while let Some(arg) = args.next() {
        match arg.as_str() {
            "--no-browser" => open_browser = false,
            "--no-readme" => exclude_readme = true,
            "--no-md" => exclude_markdown = true,
            "--gr" => {
                let Some(selected) = args.next() else {
                    eprintln!("error: --gr requires a custom graph file\n\n{USAGE}");
                    process::exit(2);
                };
                if graph_path.replace(PathBuf::from(selected)).is_some() {
                    eprintln!("error: only one custom graph file may be provided\n\n{USAGE}");
                    process::exit(2);
                }
            }
            "--path" => {
                let Some(selected) = args.next() else {
                    eprintln!("error: --path requires a directory\n\n{USAGE}");
                    process::exit(2);
                };
                if path_provided {
                    eprintln!("error: only one path may be provided\n\n{USAGE}");
                    process::exit(2);
                }
                path = PathBuf::from(selected);
                path_provided = true;
            }
            "--model" => {
                let Some(selected) = args.next() else {
                    eprintln!("error: --model requires a model ID\n\n{USAGE}");
                    process::exit(2);
                };
                if !AVAILABLE_MODELS.contains(&selected.as_str()) {
                    eprintln!(
                        "error: unsupported model '{selected}'. Choose one of: {}\n\n{USAGE}",
                        AVAILABLE_MODELS.join(", ")
                    );
                    process::exit(2);
                }
                model = AVAILABLE_MODELS
                    .iter()
                    .copied()
                    .find(|available| *available == selected)
                    .expect("selected model was checked against available models");
            }
            "-h" | "--help" => {
                println!("{USAGE}");
                return;
            }
            "-V" | "--version" => {
                println!("hello-cli {}", env!("CARGO_PKG_VERSION"));
                return;
            }
            _ if arg.starts_with('-') => {
                eprintln!("error: unrecognized argument '{arg}'\n\n{USAGE}");
                process::exit(2);
            }
            _ if path_provided => {
                eprintln!("error: only one path may be provided\n\n{USAGE}");
                process::exit(2);
            }
            _ => {
                path = PathBuf::from(arg);
                path_provided = true;
            }
        }
    }

    if let Some(graph_path) = graph_path {
        let (graph, source) = match hello_cli::graph_file::read_graph_file(&graph_path) {
            Ok(graph) => graph,
            Err(error) => {
                eprintln!("error: {error}");
                process::exit(1);
            }
        };
        let page_path = match hello_cli::page::write_graph_page(&graph, &source) {
            Ok(path) => path,
            Err(error) => {
                eprintln!("error: could not write graph page: {error}");
                process::exit(1);
            }
        };
        if open_browser && let Err(error) = hello_cli::page::open_in_browser(page_path) {
            eprintln!("error: could not open graph page in browser: {error}");
            process::exit(1);
        }
        return;
    }

    let system_prompt = match hello_cli::files::read_directory_with_options(
        &path,
        hello_cli::files::ReadOptions {
            exclude_readme,
            exclude_markdown,
        },
    ) {
        Ok(contents) => contents,
        Err(error) => {
            eprintln!("error: could not read {}: {error}", path.display());
            process::exit(1);
        }
    };

    let llm = match OpenRouter::from_env(model) {
        Ok(llm) => llm,
        Err(error) => {
            eprintln!("error: could not configure OpenRouter: {error}");
            process::exit(1);
        }
    };

    let mut steps = StepStack::new(system_prompt);
    let mut step_id = match steps.call(&llm, vec![Message::user(hello_cli::prompts::nodegraph())]) {
        Ok(step_id) => step_id,
        Err(error) => {
            eprintln!("error: OpenRouter request failed: {error}");
            process::exit(1);
        }
    };

    let mut retries = 0;
    loop {
        let output = &steps.step(step_id).unwrap().response.content;
        let correction_prompt = match hello_cli::graph_parser::validate(output) {
            Ok(()) => break,
            Err(diagnostic) => {
                let prompt = match diagnostic.kind {
                    hello_cli::graph_parser::DiagnosticKind::Syntax => {
                        include_str!("../prompts/syntax_error.txt")
                    }
                    hello_cli::graph_parser::DiagnosticKind::RedundantConnection => {
                        include_str!("../prompts/redundant_connection.txt")
                    }
                };
                format!("{prompt}\n\n{}", diagnostic.correction_details())
            }
        };
        if retries == MAX_RETRIES {
            eprintln!("final fail");
            process::exit(1);
        }
        println!("retry");
        step_id = match steps.call(&llm, vec![Message::user(correction_prompt)]) {
            Ok(step_id) => step_id,
            Err(error) => {
                eprintln!("error: OpenRouter request failed: {error}");
                process::exit(1);
            }
        };
        retries += 1;
    }

    let page_path = match hello_cli::page::write_response_history_page(steps.steps()) {
        Ok(path) => path,
        Err(error) => {
            eprintln!("error: could not write response page: {error}");
            process::exit(1);
        }
    };

    if open_browser && let Err(error) = hello_cli::page::open_in_browser(page_path) {
        eprintln!("error: could not open response page in browser: {error}");
        process::exit(1);
    }
}

#[cfg(test)]
mod tests {
    #[test]
    fn correction_prompts_are_distinct() {
        assert!(include_str!("../prompts/syntax_error.txt").contains("syntax"));
        assert!(include_str!("../prompts/redundant_connection.txt").contains("connection"));
    }
}
