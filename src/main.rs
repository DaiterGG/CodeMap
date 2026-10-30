//! CLI orchestration: collect source files, request a custom graph from the LLM,
//! validate and retry its output, then export the final graph as HTML and files.
//! `--gr FILE` skips collection and LLM work and renders the supplied graph file.

use std::env;
use std::fs;
use std::path::PathBuf;
use std::process;

use hello_cli::llm::{OpenRouter, list_model_ids};
use hello_cli::step::{ChatClient, Message, StepStack};

const DEFAULT_MODEL: &str = "liquid/lfm-2.5-2.6b:free";
const DEFAULT_PATH: &str = "examples/small_repo";
const MAX_RETRIES: usize = 3;

fn call_with_retries(
    steps: &mut StepStack,
    llm: &dyn ChatClient,
    input: Vec<Message>,
) -> Result<hello_cli::step::StepId, String> {
    let mut retries = 0;
    loop {
        match steps.call(llm, input.clone()) {
            Ok(step_id) => return Ok(step_id),
            Err(error) => {
                if retries == MAX_RETRIES {
                    return Err(format!(
                        "model request failed after {MAX_RETRIES} retries: {error}"
                    ));
                }
                eprintln!("retry: model request failed: {error}");
                retries += 1;
            }
        }
    }
}

const USAGE: &str = "Usage: hello-cli [OPTIONS]

Read files under PATH and ask the configured model a question.

Options:
      --path PATH  Directory to read (default: examples/small_repo)
      --test       Use hyperqwen/qwen3.8-27b with TEST_TOKEN
      --web        Host the web page on localhost and open it in a browser
      --server     Serve the web page on all interfaces at port 36800
      --models     List all OpenRouter model IDs
      --prompt     Write the initial model request to prompt.txt
      --gr FILE    Render a custom graph file as HTML and open it in a browser
      --no-readme  Exclude files named README (case-insensitive, any extension)
      --no-md      Exclude Markdown files (.md and .markdown)
      --non-txt    Include non-text files as base64-encoded content
      --no-browser  Write the response page without opening a browser
      --model ID   OpenRouter model ID (default: liquid/lfm-2.5-2.6b:free)
  -h, --help       Print help
  -V, --version    Print version";

fn main() {
    let mut path = PathBuf::from(DEFAULT_PATH);
    let mut path_provided = false;
    let mut graph_path = None;
    let mut web_mode = false;
    let mut server_mode = false;
    let mut models_mode = false;
    let mut write_prompt = false;
    let mut test_mode = false;
    let mut model = DEFAULT_MODEL.to_owned();
    let mut model_provided = false;
    let mut open_browser = true;
    let mut exclude_readme = false;
    let mut exclude_markdown = false;
    let mut include_non_text = false;
    let mut args = env::args().skip(1);
    while let Some(arg) = args.next() {
        match arg.as_str() {
            "--no-browser" => open_browser = false,
            "--web" => web_mode = true,
            "--server" => server_mode = true,
            "--models" => models_mode = true,
            "--prompt" => write_prompt = true,
            "--test" => test_mode = true,
            "--no-readme" => exclude_readme = true,
            "--no-md" => exclude_markdown = true,
            "--non-txt" => include_non_text = true,
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
                if selected.trim().is_empty() || selected.starts_with('-') {
                    eprintln!("error: --model requires a valid model ID\n\n{USAGE}");
                    process::exit(2);
                }
                if model_provided {
                    eprintln!("error: only one model may be provided\n\n{USAGE}");
                    process::exit(2);
                }
                model = selected;
                model_provided = true;
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

    if web_mode && server_mode {
        eprintln!("error: --web and --server cannot be used together\n\n{USAGE}");
        process::exit(2);
    }

    if models_mode
        && (web_mode
            || server_mode
            || test_mode
            || model_provided
            || graph_path.is_some()
            || path_provided)
    {
        eprintln!(
            "error: --models cannot be combined with --web, --server, --test, --model, --gr, or a path\n\n{USAGE}"
        );
        process::exit(2);
    }

    if model_provided && (web_mode || server_mode) {
        eprintln!("error: --model cannot be used with --web or --server\n\n{USAGE}");
        process::exit(2);
    }

    if test_mode && model_provided {
        eprintln!("error: --test selects its own model and cannot be used with --model\n\n{USAGE}");
        process::exit(2);
    }

    if test_mode && (web_mode || server_mode) {
        eprintln!("error: --test cannot be used with --web or --server\n\n{USAGE}");
        process::exit(2);
    }

    if models_mode {
        match list_model_ids() {
            Ok(model_ids) => {
                for model_id in model_ids {
                    println!("{model_id}");
                }
            }
            Err(error) => {
                eprintln!("error: could not fetch OpenRouter models: {error}");
                process::exit(1);
            }
        }
        return;
    }

    if server_mode {
        if let Err(error) = hello_cli::web::run_server_mode() {
            eprintln!("error: server mode failed: {error}");
            process::exit(1);
        }
        return;
    }

    if web_mode {
        if let Err(error) = hello_cli::web::run() {
            eprintln!("error: web mode failed: {error}");
            process::exit(1);
        }
        return;
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

    let source_files = match hello_cli::files::read_directory_files_with_options(
        &path,
        hello_cli::files::ReadOptions {
            exclude_readme,
            exclude_markdown,
            include_non_text,
        },
    ) {
        Ok(contents) => contents,
        Err(error) => {
            eprintln!("error: could not read {}: {error}", path.display());
            process::exit(1);
        }
    };

    let llm: Box<dyn ChatClient> = if test_mode {
        match hello_cli::test_model::TestModel::from_env() {
            Ok(llm) => Box::new(llm),
            Err(error) => {
                eprintln!("error: could not configure test model: {error}");
                process::exit(1);
            }
        }
    } else {
        match OpenRouter::from_env(model) {
            Ok(llm) => Box::new(llm),
            Err(error) => {
                eprintln!("error: could not configure OpenRouter: {error}");
                process::exit(1);
            }
        }
    };

    let mut steps = StepStack::with_files(source_files);
    let initial_prompt = vec![Message::user(hello_cli::prompts::nodegraph())];
    let output_dir = match hello_cli::page::create_response_history_dir() {
        Ok(path) => path,
        Err(error) => {
            eprintln!("error: could not create response output directory: {error}");
            process::exit(1);
        }
    };
    if write_prompt {
        let mut messages = steps.context();
        messages.extend(initial_prompt.iter().cloned());
        let prompt_text = format_prompt_messages(&messages);
        let prompt_path = output_dir.join("prompt.txt");
        if let Err(error) = fs::write(&prompt_path, prompt_text) {
            eprintln!("error: could not write {}: {error}", prompt_path.display());
            process::exit(1);
        }
    }
    let mut step_id = match call_with_retries(&mut steps, llm.as_ref(), initial_prompt) {
        Ok(step_id) => step_id,
        Err(error) => {
            eprintln!("error: {error}");
            process::exit(1);
        }
    };

    let mut validation_errors = Vec::new();
    let mut retries = 0;
    loop {
        validation_errors.push(None);
        if let Err(error) = hello_cli::page::write_response_history_snapshot(
            &output_dir,
            steps.steps(),
            &validation_errors,
        ) {
            eprintln!("error: could not write response history: {error}");
            process::exit(1);
        }

        let output = &steps.step(step_id).unwrap().response.content;
        let (correction_prompt, validation_error) = match hello_cli::graph_parser::validate(output)
        {
            Ok(()) => break,
            Err(diagnostic) => {
                let validation_error = format!(
                    "Graph validation failed ({:?}):\n{}",
                    diagnostic.kind,
                    diagnostic.correction_details()
                );
                *validation_errors.last_mut().unwrap() = Some(validation_error.clone());
                if let Err(error) = hello_cli::page::write_response_history_snapshot(
                    &output_dir,
                    steps.steps(),
                    &validation_errors,
                ) {
                    eprintln!("error: could not update response history: {error}");
                    process::exit(1);
                }
                let prompt = match diagnostic.kind {
                    hello_cli::graph_parser::DiagnosticKind::Syntax => {
                        include_str!("../prompts/syntax_error.txt")
                    }
                    hello_cli::graph_parser::DiagnosticKind::RedundantConnection => {
                        include_str!("../prompts/redundant_connection.txt")
                    }
                };
                (
                    format!("{prompt}\n\n{}", diagnostic.correction_details()),
                    validation_error,
                )
            }
        };
        if retries == MAX_RETRIES {
            eprintln!("final fail: {validation_error}");
            process::exit(1);
        }
        println!("retry: {validation_error}");
        step_id = match call_with_retries(
            &mut steps,
            llm.as_ref(),
            vec![Message::user(correction_prompt)],
        ) {
            Ok(step_id) => step_id,
            Err(error) => {
                eprintln!("error: {error}");
                process::exit(1);
            }
        };
        retries += 1;
    }

    let page_path = match hello_cli::page::finalize_response_history_page_with_sources(
        &output_dir,
        steps.steps(),
        &validation_errors,
        Some(&path),
    ) {
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

fn format_prompt_messages(messages: &[Message]) -> String {
    let mut output = String::new();
    for message in messages {
        let role = match message.role {
            hello_cli::step::Role::System => "system",
            hello_cli::step::Role::User => "user",
            hello_cli::step::Role::Assistant => "assistant",
        };
        output.push_str(&format!("<{role}>\n{}\n</{role}>\n", message.content));
    }
    output
}

#[cfg(test)]
mod tests {
    use super::{call_with_retries, format_prompt_messages};
    use hello_cli::step::{ChatClient, Message, StepStack};
    use std::cell::Cell;
    use std::error::Error;

    struct FailsThenSucceeds {
        failures_left: Cell<usize>,
    }

    impl ChatClient for FailsThenSucceeds {
        fn complete(&self, _messages: &[Message]) -> Result<Message, Box<dyn Error + Send + Sync>> {
            let failures_left = self.failures_left.get();
            if failures_left > 0 {
                self.failures_left.set(failures_left - 1);
                return Err("temporary model error".into());
            }
            Ok(Message::assistant("response"))
        }
    }

    #[test]
    fn retries_failed_model_calls_until_success() {
        let llm = FailsThenSucceeds {
            failures_left: Cell::new(1),
        };
        let mut steps = StepStack::new("system");

        assert!(call_with_retries(&mut steps, &llm, vec![Message::user("prompt")]).is_ok());
        assert_eq!(steps.steps().len(), 1);
    }

    #[test]
    fn reports_the_model_error_after_exhausting_retries() {
        let llm = FailsThenSucceeds {
            failures_left: Cell::new(usize::MAX),
        };
        let mut steps = StepStack::new("system");

        let error = call_with_retries(&mut steps, &llm, vec![Message::user("prompt")])
            .expect_err("the model should keep failing");

        assert!(error.contains("after 3 retries"));
        assert!(error.contains("temporary model error"));
        assert!(steps.steps().is_empty());
    }

    #[test]
    fn prompt_transcript_keeps_message_text_and_roles() {
        let messages = [
            Message::system("source context\n"),
            Message::user("Generate a graph."),
        ];

        assert_eq!(
            format_prompt_messages(&messages),
            "<system>\nsource context\n\n</system>\n<user>\nGenerate a graph.\n</user>\n"
        );
    }

    #[test]
    fn correction_prompts_are_distinct() {
        assert!(include_str!("../prompts/syntax_error.txt").contains("syntax"));
        assert!(include_str!("../prompts/redundant_connection.txt").contains("connection"));
    }
}
