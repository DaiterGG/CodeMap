//! Serializes validated graphs and writes browser pages and response history.
//! Mermaid source export lives separately.

use crate::graph_parser::{Graph, GraphDirection};
use crate::step::{Role, Step};
use serde::Serialize;
use std::collections::{HashMap, HashSet};
use std::fs;
use std::io;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::time::{SystemTime, UNIX_EPOCH};

const CODEMAP_WASM: &[u8] = include_bytes!(concat!(env!("OUT_DIR"), "/codemap-core.wasm"));
const CODEMAP_BINDINGS_JS: &str = include_str!(concat!(env!("OUT_DIR"), "/codemap_core.js"));
const NODE_WIDTH: f64 = 210.0;
const NODE_HEIGHT: f64 = 131.0;

pub fn write_response_page(_thought: Option<&str>, response: &str) -> io::Result<PathBuf> {
    let output_dir = Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("target")
        .join("generated");
    write_response_files(&output_dir, response)
}

pub fn write_graph_page(graph: &Graph, graph_source: &str) -> io::Result<PathBuf> {
    let graph_json = serialize_graph_document(graph)?;
    let html = render_page_html(graph, &graph_json, "{}");
    let generated_dir = Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("target")
        .join("generated");
    fs::create_dir_all(&generated_dir)?;
    let output_dir = create_timestamped_dir(&generated_dir)?;
    let output_path = output_dir.join("response.html");
    fs::write(&output_path, html)?;
    crate::mermaid::write_graph_file(&output_dir, graph)?;
    crate::graph_file::write_graph_file(&output_dir, graph_source)?;
    Ok(output_path)
}

pub fn write_response_history_page(history: &[Step]) -> io::Result<PathBuf> {
    let output_dir = create_response_history_dir()?;
    finalize_response_history_page(&output_dir, history, &[])
}

pub fn create_response_history_dir() -> io::Result<PathBuf> {
    let output_dir = Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("target")
        .join("generated");
    fs::create_dir_all(&output_dir)?;
    create_timestamped_dir(&output_dir)
}

pub fn write_web_generation_files(graph: &Graph, graph_source: &str) -> io::Result<PathBuf> {
    let generated_dir = Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("target")
        .join("generated");
    fs::create_dir_all(&generated_dir)?;
    let output_dir = create_timestamped_dir(&generated_dir)?;
    write_graph_artifacts(&output_dir, graph, graph_source)?;
    Ok(output_dir)
}

fn write_graph_artifacts(output_dir: &Path, graph: &Graph, graph_source: &str) -> io::Result<()> {
    crate::mermaid::write_graph_file(output_dir, graph)?;
    crate::graph_file::write_graph_file(output_dir, graph_source)?;
    Ok(())
}

pub fn write_response_history_snapshot(
    output_dir: &Path,
    history: &[Step],
    validation_errors: &[Option<String>],
) -> io::Result<()> {
    fs::write(
        output_dir.join("response.txt"),
        format_history(history, validation_errors),
    )
}

pub fn finalize_response_history_page(
    output_dir: &Path,
    history: &[Step],
    validation_errors: &[Option<String>],
) -> io::Result<PathBuf> {
    finalize_response_history_page_with_sources(output_dir, history, validation_errors, None)
}

pub fn finalize_response_history_page_with_sources(
    output_dir: &Path,
    history: &[Step],
    _validation_errors: &[Option<String>],
    source_root: Option<&Path>,
) -> io::Result<PathBuf> {
    let Some(final_step) = history.last() else {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            "cannot write response history without an LLM response",
        ));
    };
    let graph = crate::graph_file::parse_graph(&final_step.response.content)?;
    let graph_json = serialize_graph_document(&graph)?;
    let source_json = serialize_embedded_sources(&graph, source_root)?;
    let html = render_page_html(&graph, &graph_json, &source_json);
    let output_path = output_dir.join("response.html");
    fs::write(&output_path, html)?;
    crate::mermaid::write_graph_file(output_dir, &graph)?;
    crate::graph_file::write_graph_file(
        output_dir,
        crate::graph_file::normalize_graph_source(&final_step.response.content),
    )?;
    Ok(output_path)
}

fn serialize_embedded_sources(graph: &Graph, source_root: Option<&Path>) -> io::Result<String> {
    let Some(source_root) = source_root else {
        return Ok("{}".to_owned());
    };
    let root = source_root.canonicalize()?;
    let references = graph
        .nodes
        .iter()
        .flat_map(|node| node.sources.iter())
        .collect::<HashSet<_>>();
    let mut sources = HashMap::new();
    for reference in references {
        let relative_path = Path::new(&reference.path);
        if relative_path.is_absolute()
            || relative_path
                .components()
                .any(|component| !matches!(component, std::path::Component::Normal(_)))
        {
            return Err(io::Error::new(
                io::ErrorKind::InvalidInput,
                "source reference path must stay inside the project",
            ));
        }
        let path = root.join(relative_path).canonicalize()?;
        if !path.starts_with(&root) || !path.is_file() {
            return Err(io::Error::new(
                io::ErrorKind::NotFound,
                format!("source reference is not a project file: {}", reference.path),
            ));
        }
        let source = fs::read_to_string(path)?;
        sources.insert(reference.path.clone(), source);
    }
    let json = serde_json::to_string(&sources).map_err(io::Error::other)?;
    Ok(json.replace('<', "\\u003c"))
}

fn write_response_files(generated_dir: &Path, response: &str) -> io::Result<PathBuf> {
    let graph = crate::graph_file::parse_graph(response)?;
    let graph_json = serialize_graph_document(&graph)?;
    let html = render_page_html(&graph, &graph_json, "{}");
    let raw_graph = crate::graph_file::normalize_graph_source(response);

    fs::create_dir_all(generated_dir)?;
    let output_dir = create_timestamped_dir(generated_dir)?;
    let output_path = output_dir.join("response.html");
    fs::write(&output_path, html)?;
    crate::mermaid::write_graph_file(&output_dir, &graph)?;
    crate::graph_file::write_graph_file(&output_dir, raw_graph)?;
    Ok(output_path)
}

#[derive(Serialize)]
struct GraphDocument {
    direction: &'static str,
    name: Option<String>,
    description: Option<String>,
    width: f64,
    height: f64,
    nodes: Vec<GraphNodeData>,
    edges: Vec<PositionedEdge>,
}

#[derive(Serialize)]
struct GraphNodeData {
    id: String,
    name: Option<String>,
    description: Option<String>,
    sources: Vec<crate::graph_parser::SourceReference>,
    width: f64,
    height: f64,
}

#[derive(Serialize)]
struct PositionedEdge {
    from: String,
    to: String,
    label: String,
    dotted: bool,
    arrow_start: bool,
    arrow_end: bool,
}

pub(crate) fn serialize_graph_document(graph: &Graph) -> io::Result<String> {
    let document = GraphDocument {
        direction: match graph.direction {
            GraphDirection::TopDown => "TD",
            GraphDirection::LeftRight => "LR",
        },
        name: graph.name.clone(),
        description: graph.description.clone(),
        width: 320.0,
        height: 200.0,
        nodes: graph
            .nodes
            .iter()
            .map(|node| {
                Ok(GraphNodeData {
                    id: node.id.clone(),
                    name: node.name.clone(),
                    description: node.description.clone(),
                    sources: node.sources.clone(),
                    width: NODE_WIDTH,
                    height: NODE_HEIGHT,
                })
            })
            .collect::<io::Result<Vec<_>>>()?,
        edges: graph
            .edges
            .iter()
            .map(|edge| PositionedEdge {
                from: edge.from.clone(),
                to: edge.to.clone(),
                label: edge.label.clone(),
                dotted: edge.dotted,
                arrow_start: edge.arrow_start,
                arrow_end: edge.arrow_end,
            })
            .collect(),
    };
    let json = serde_json::to_string(&document).map_err(io::Error::other)?;
    Ok(json.replace('<', "\\u003c"))
}

fn render_page_html(graph: &Graph, graph_json: &str, source_json: &str) -> String {
    let title = graph
        .name
        .as_deref()
        .filter(|title| !title.is_empty())
        .map(|title| format!("<h1>{}</h1>", escape_html(title)))
        .unwrap_or_default();
    let description = graph
        .description
        .as_deref()
        .filter(|description| !description.is_empty())
        .map(|description| {
            format!(
                "<p class=\"graph-description\">{}</p>",
                escape_html(description)
            )
        })
        .unwrap_or_default();
    crate::page_template::render_page_html(
        &format!("{title}{description}"),
        graph_json,
        source_json,
        CODEMAP_WASM,
        CODEMAP_BINDINGS_JS,
        crate::prompts::nodegraph(),
        "",
    )
}

fn format_history(history: &[Step], validation_errors: &[Option<String>]) -> String {
    history
        .iter()
        .enumerate()
        .map(|(index, step)| {
            let correction_prompt = (index != 0).then(|| {
                step.input
                    .iter()
                    .filter(|message| message.role == Role::User)
                    .map(|message| message.content.as_str())
                    .collect::<Vec<_>>()
                    .join("\n\n")
            });
            format_attempt(
                index + 1,
                correction_prompt
                    .as_deref()
                    .filter(|prompt| !prompt.is_empty()),
                step.response.reasoning.as_deref(),
                &step.response.content,
                validation_errors.get(index).and_then(Option::as_deref),
            )
        })
        .collect()
}

fn format_attempt(
    attempt: usize,
    correction_prompt: Option<&str>,
    thought: Option<&str>,
    response: &str,
    validation_error: Option<&str>,
) -> String {
    let mut text = format!("<attempt number=\"{attempt}\">\n");
    if let Some(prompt) = correction_prompt {
        text.push_str("<error-prompt>\n");
        text.push_str(prompt);
        text.push_str("\n</error-prompt>\n");
    }
    if let Some(thought) = thought {
        text.push_str("<thinking>\n");
        text.push_str(thought);
        text.push_str("\n</thinking>\n");
    }
    text.push_str("<output>\n");
    text.push_str(response);
    text.push_str("\n</output>\n");
    if let Some(error) = validation_error {
        text.push_str("<validation-error>\n");
        text.push_str(error);
        text.push_str("\n</validation-error>\n");
    }
    text.push_str("</attempt>\n");
    text
}

fn create_timestamped_dir(generated_dir: &Path) -> io::Result<PathBuf> {
    loop {
        let timestamp = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .map_err(io::Error::other)?
            .as_nanos();
        let output_dir = generated_dir.join(timestamp.to_string());
        match fs::create_dir(&output_dir) {
            Ok(()) => return Ok(output_dir),
            Err(error) if error.kind() == io::ErrorKind::AlreadyExists => continue,
            Err(error) => return Err(error),
        }
    }
}

pub fn open_in_browser(path: impl AsRef<Path>) -> io::Result<()> {
    let path = fs::canonicalize(path)?;
    #[cfg(target_os = "windows")]
    {
        Command::new("cmd")
            .args(["/C", "start", ""])
            .arg(path)
            .spawn()?;
    }
    #[cfg(target_os = "macos")]
    {
        Command::new("open").arg(path).spawn()?;
    }
    #[cfg(all(unix, not(target_os = "macos")))]
    {
        Command::new("xdg-open").arg(path).spawn()?;
    }
    Ok(())
}

fn escape_html(value: &str) -> String {
    let mut escaped = String::with_capacity(value.len());
    for character in value.chars() {
        match character {
            '&' => escaped.push_str("&amp;"),
            '<' => escaped.push_str("&lt;"),
            '>' => escaped.push_str("&gt;"),
            '"' => escaped.push_str("&quot;"),
            '\'' => escaped.push_str("&#39;"),
            _ => escaped.push(character),
        }
    }
    escaped
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::graph_parser::parse;

    const GRAPH: &str = r#"<graph type="LR" name="Test graph" desc="Shows an input, transformation, and output">
Input["<name>Input</name><desc>Read source</desc>"]
Process["<name>Process</name><desc>Transform source</desc>"]
Output["<desc>Write output</desc>"]
Input --> Process
Process -.->|optional| Output
</graph>
"#;

    #[test]
    fn escapes_html_in_text() {
        assert_eq!(
            escape_html("<script a=\"b\">Tom & Jerry's</script>"),
            "&lt;script a=&quot;b&quot;&gt;Tom &amp; Jerry&#39;s&lt;/script&gt;"
        );
    }

    #[test]
    fn static_page_splits_default_key_into_escaped_json_parts() {
        let default_key = "test-default-key";
        let html = crate::page_template::render_page_html(
            "",
            "{}",
            "{}",
            b"wasm",
            "bindings",
            "prompt",
            default_key,
        );

        assert!(html.contains(r#""test-def""#));
        assert!(html.contains(r#""ault-key""#));
        assert!(!html.contains(default_key));
        assert!(html.contains("default-api-key-a"));
        assert!(html.contains("default-api-key-b"));
        assert!(!html.contains("{{DEFAULT_API_KEY_FIRST_JSON}}"));
        assert!(!html.contains("{{DEFAULT_API_KEY_SECOND_JSON}}"));
    }

    #[test]
    fn static_page_splits_unicode_default_key_on_character_boundary() {
        let html = crate::page_template::render_page_html(
            "",
            "{}",
            "{}",
            b"wasm",
            "bindings",
            "prompt",
            "abc🔑def",
        );

        assert!(html.contains(r#""abc""#));
        assert!(html.contains(r#""🔑def""#));
    }

    #[test]
    fn summary_controls_keep_compact_styles_and_selected_model_label() {
        let html = crate::page_template::render_page_html(
            "", "{}", "{}", b"wasm", "bindings", "prompt", "",
        );

        assert!(html.contains(".node-summary-actions .node-summary-tool-button"));
        assert!(html.contains("border: 0;"));
        assert!(html.contains("summaryInput.value = \"Generate node summary\""));
        assert!(html.contains("summaryInput.focus()"));
        assert!(html.contains("const selectedModelLabel = selectedModel ? modelDisplayName(selectedModel) : \"Models\";"));
        assert!(html.contains("generationSettingsButton.textContent = \"Settings\""));
        assert!(html.contains(".api-auth-controls {\n      display: flex;\n      flex-direction: column;\n      align-items: stretch;\n      width: 100%;"));
        assert!(html.contains("#openrouter-sign-in-button {\n      width: 100%;"));
        assert!(html.contains(".api-status-row {\n      box-sizing: border-box;\n      width: 100%;\n      justify-content: space-between;"));
        assert!(html.contains(
            "generateSummaryButton.setAttribute(\"aria-label\", \"Generate node summary\")"
        ));
        assert!(!html.contains("Regenerate node summary"));
        assert!(!html.contains("generateSummaryButton.textContent ="));
    }

    #[test]
    fn graph_transfer_ui_supports_gr_files_only() {
        let html = crate::page_template::render_page_html(
            "", "{}", "{}", b"wasm", "bindings", "prompt", "",
        );

        assert!(html.contains("Import a graph file (.gr)"));
        assert!(html.contains("accept=\".gr,text/plain\""));
        assert!(html.contains("Download graph.gr"));
        assert!(!html.contains("export-graph-json"));
        assert!(!html.contains("Download graph.json"));
        assert!(!html.contains(".json graph file"));
        assert!(!html.contains("graphJsonForExport"));
        assert!(!html.contains("prepareGraphForDisplay"));
    }

    #[test]
    fn exports_graph_data_for_elks_initial_layout() {
        let graph_source = GRAPH.replace(
            "<name>Input</name><desc>Read source</desc>",
            "<name>Input</name><desc>Read source</desc><source>src/main.rs:4-8</source>",
        );
        let graph = parse(&graph_source).unwrap();
        let json = serialize_graph_document(&graph).unwrap();
        let document: serde_json::Value = serde_json::from_str(&json).unwrap();

        assert_eq!(document["direction"], "TD");
        assert_eq!(document["name"], "Test graph");
        assert_eq!(document["nodes"].as_array().unwrap().len(), 3);
        assert_eq!(document["nodes"][0]["id"], "Input");
        assert!(document["nodes"][0].get("x").is_none());
        assert!(document["nodes"][0].get("y").is_none());
        assert_eq!(document["nodes"][0]["width"], NODE_WIDTH);
        assert_eq!(document["nodes"][0]["height"], NODE_HEIGHT);
        assert_eq!(document["nodes"][0]["sources"][0]["path"], "src/main.rs");
        assert_eq!(document["nodes"][0]["sources"][0]["start"], 4);
        assert_eq!(document["nodes"][0]["sources"][0]["end"], 8);
        assert_eq!(document["edges"].as_array().unwrap().len(), 2);
        assert_eq!(document["edges"][1]["label"], "optional");
        assert_eq!(document["edges"][1]["dotted"], true);
        assert_eq!(document["edges"][1]["arrow_end"], true);
    }

    #[test]
    fn embeds_referenced_source_files_for_cli_pages() {
        let unique = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .as_nanos();
        let root = std::env::temp_dir().join(format!("hello-cli-embedded-source-{unique}"));
        fs::create_dir_all(root.join("src")).unwrap();
        fs::write(root.join("src/main.rs"), "fn main() {}\n").unwrap();
        let graph_source = GRAPH.replace(
            "<name>Input</name><desc>Read source</desc>",
            "<name>Input</name><desc>Read source</desc><source>src/main.rs:1-1</source>",
        );
        let graph = parse(&graph_source).unwrap();
        let embedded = serialize_embedded_sources(&graph, Some(&root)).unwrap();
        let sources: serde_json::Value = serde_json::from_str(&embedded).unwrap();

        assert_eq!(sources["src/main.rs"], "fn main() {}\n");
        fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn web_generation_exports_graph_files_without_response_page() {
        let unique = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .as_nanos();
        let output_dir = std::env::temp_dir().join(format!("hello-cli-web-export-{unique}"));
        fs::create_dir_all(&output_dir).unwrap();
        let graph = parse(GRAPH).unwrap();
        write_graph_artifacts(&output_dir, &graph, GRAPH).unwrap();

        assert!(!output_dir.join("graph.json").exists());
        assert!(output_dir.join("graph.mmd").is_file());
        assert!(output_dir.join("graph.gr").is_file());
        assert!(!output_dir.join("response.txt").exists());
        assert!(!output_dir.join("response.html").exists());
        fs::remove_dir_all(output_dir).unwrap();
    }

    #[test]
    fn writes_standalone_graph_html_page() {
        let graph = parse(GRAPH).unwrap();
        let path = write_graph_page(&graph, GRAPH).unwrap();

        assert_eq!(path.file_name().unwrap(), "response.html");
        assert!(!path.with_file_name("graph.json").exists());
        assert!(path.with_file_name("graph.mmd").exists());
        fs::remove_dir_all(path.parent().unwrap()).unwrap();
    }

    #[test]
    fn embeds_positioned_graph_data_in_final_response_history_page() {
        use crate::step::{Message, StepId};

        let unique = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .as_nanos();
        let output_dir = std::env::temp_dir().join(format!("hello-cli-final-json-{unique}"));
        fs::create_dir_all(&output_dir).unwrap();
        let history = vec![Step {
            id: StepId(0),
            input: vec![Message::user("generate a graph")],
            response: Message::assistant(GRAPH),
        }];

        let html_path = finalize_response_history_page(&output_dir, &history, &[]).unwrap();
        let graph_json = serialize_graph_document(&parse(GRAPH).unwrap()).unwrap();
        let document: serde_json::Value = serde_json::from_str(&graph_json).unwrap();
        let html = fs::read_to_string(html_path).unwrap();

        assert_eq!(document["nodes"].as_array().unwrap().len(), 3);
        assert_eq!(document["edges"].as_array().unwrap().len(), 2);
        assert!(html.contains(&graph_json));
        assert!(!output_dir.join("graph.json").exists());
        assert!(!output_dir.join("response.txt").exists());
        fs::remove_dir_all(output_dir).unwrap();
    }

    #[test]
    fn graph_export_strips_outer_markdown_fences() {
        let fenced_graph = format!("```mermaid\n{GRAPH}```");
        assert_eq!(
            crate::graph_file::normalize_graph_source(&fenced_graph),
            GRAPH.trim()
        );
    }

    #[test]
    fn response_history_records_each_retry_without_the_source_context() {
        use crate::step::{Message, StepId};

        let history = vec![
            Step {
                id: StepId(0),
                input: vec![Message::user("initial graph request")],
                response: Message {
                    role: Role::Assistant,
                    content: "first graph attempt".to_owned(),
                    reasoning: Some("First attempt thought".to_owned()),
                },
            },
            Step {
                id: StepId(1),
                input: vec![Message::user(
                    "Error\nFix the issue on line 4: missing node declaration.",
                )],
                response: Message {
                    role: Role::Assistant,
                    content: GRAPH.to_owned(),
                    reasoning: Some("Retry thought".to_owned()),
                },
            },
        ];

        let text = format_history(
            &history,
            &[
                Some("Graph validation failed: missing node declaration.".to_owned()),
                None,
            ],
        );
        let first_output = text.find("first graph attempt").unwrap();
        let validation_error = text.find("<validation-error>").unwrap();
        let correction = text.find("<error-prompt>").unwrap();
        let retry_thought = text.find("Retry thought").unwrap();
        let retry_output = text.find(GRAPH).unwrap();

        assert!(first_output < correction);
        assert!(validation_error < correction);
        assert!(correction < retry_thought);
        assert!(retry_thought < retry_output);
        assert!(text.contains("Fix the issue on line 4"));
        assert!(!text.contains("&lt;name&gt;"));
        assert_eq!(text.matches("<attempt number=").count(), 2);
        assert!(!text.contains("initial graph request"));
        assert!(!text.contains("source context"));
    }

    #[test]
    fn response_history_snapshot_writes_unvalidated_output_and_errors() {
        use crate::step::{Message, StepId};

        let unique = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .as_nanos();
        let output_dir = std::env::temp_dir().join(format!("hello-cli-history-{unique}"));
        fs::create_dir_all(&output_dir).unwrap();
        let history = vec![Step {
            id: StepId(0),
            input: vec![Message::user("generate a graph")],
            response: Message::assistant("invalid first answer"),
        }];

        write_response_history_snapshot(
            &output_dir,
            &history,
            &[Some("Syntax error on line 2.".to_owned())],
        )
        .unwrap();

        let text = fs::read_to_string(output_dir.join("response.txt")).unwrap();
        assert!(text.contains("invalid first answer"));
        assert!(text.contains("Syntax error on line 2."));
        fs::remove_dir_all(output_dir).unwrap();
    }

    #[test]
    fn rejects_invalid_graphs_before_writing_pages() {
        let unique = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .as_nanos();
        let generated_dir = std::env::temp_dir().join(format!("hello-cli-invalid-{unique}"));
        let error = write_response_files(&generated_dir, "not a graph").unwrap_err();
        assert_eq!(error.kind(), io::ErrorKind::InvalidData);
        assert!(!generated_dir.exists());
    }
}
