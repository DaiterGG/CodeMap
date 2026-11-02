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

const TEMPLATE: &str = include_str!("../public/index.html");
const GRAPH_JSON_MARKER: &str = "{{GRAPH_JSON}}";
const SOURCE_JSON_MARKER: &str = "{{SOURCE_JSON}}";
const ELK_JS_MARKER: &str = "{{ELK_JS}}";
const ELK_LICENSE_MARKER: &str = "{{ELK_LICENSE}}";
const GRAPH_DETAILS_MARKER: &str = "{{GRAPH_DETAILS}}";
const SERVER_MODE_MARKER: &str = "{{SERVER_MODE}}";
const HIGHLIGHT_JS_MARKER: &str = "{{HIGHLIGHT_JS}}";
const HIGHLIGHT_JS: &str = include_str!("../public/vendor/highlight.min.js");
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
    let html = render_page_html(graph, &graph_json, "{}", false);
    let generated_dir = Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("target")
        .join("generated");
    fs::create_dir_all(&generated_dir)?;
    let output_dir = create_timestamped_dir(&generated_dir)?;
    let output_path = output_dir.join("response.html");
    fs::write(&output_path, html)?;
    fs::write(output_dir.join("graph.json"), &graph_json)?;
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
    let graph_json = serialize_graph_document(graph)?;
    fs::write(output_dir.join("graph.json"), graph_json)?;
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
    let html = render_page_html(&graph, &graph_json, &source_json, false);
    let output_path = output_dir.join("response.html");
    fs::write(&output_path, html)?;
    fs::write(output_dir.join("graph.json"), &graph_json)?;
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
    let html = render_page_html(&graph, &graph_json, "{}", false);
    let raw_graph = crate::graph_file::normalize_graph_source(response);

    fs::create_dir_all(generated_dir)?;
    let output_dir = create_timestamped_dir(generated_dir)?;
    let output_path = output_dir.join("response.html");
    fs::write(&output_path, html)?;
    fs::write(output_dir.join("graph.json"), &graph_json)?;
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

fn render_page_html(
    graph: &Graph,
    graph_json: &str,
    source_json: &str,
    server_mode: bool,
) -> String {
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
    TEMPLATE
        .replace(GRAPH_DETAILS_MARKER, &format!("{title}{description}"))
        .replace(GRAPH_JSON_MARKER, graph_json)
        .replace(SOURCE_JSON_MARKER, source_json)
        .replace(
            ELK_JS_MARKER,
            include_str!("../public/vendor/elk.bundled.js"),
        )
        .replace(
            ELK_LICENSE_MARKER,
            &escape_html(include_str!("../public/vendor/elk.LICENSE")),
        )
        .replace(HIGHLIGHT_JS_MARKER, HIGHLIGHT_JS)
        .replace(
            SERVER_MODE_MARKER,
            if server_mode { "true" } else { "false" },
        )
}

pub(crate) fn render_web_index(server_mode: bool) -> String {
    let graph = Graph {
        direction: GraphDirection::TopDown,
        name: None,
        description: None,
        nodes: Vec::new(),
        edges: Vec::new(),
        partials: Vec::new(),
    };
    let graph_json =
        serialize_graph_document(&graph).expect("an empty graph can always be serialized");
    render_page_html(&graph, &graph_json, "{}", server_mode)
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
    fn web_index_leaves_graph_metadata_empty_for_javascript() {
        let html = render_web_index(false);
        assert!(html.contains("<div class=\"graph-details\" id=\"graph-details\"></div>"));
        assert!(!html.contains("<h1>CodeMap</h1>"));
        assert!(!html.contains("{{GRAPH_DETAILS}}"));
        assert!(html.contains("data-server-mode=\"false\""));
        assert!(html.contains("id=\"generation-settings-button\""));
        assert!(html.contains("#model-select:disabled"));
        assert!(html.contains("id=\"generation-settings-dialog\""));
        assert!(html.contains("id=\"include-gitignored\" type=\"checkbox\""));
        assert!(html.contains("id=\"include-non-text\" type=\"checkbox\""));
        assert!(html.contains("id=\"ignore-big-files\" type=\"checkbox\""));
        assert!(html.contains("ignore_big_files: ignoreBigFiles.checked"));
        assert!(html.contains("id=\"openrouter-sign-in-button\""));
    }

    #[test]
    fn server_web_index_is_marked_for_the_browser() {
        let html = render_web_index(true);
        assert!(html.contains("data-server-mode=\"true\""));
        assert!(!html.contains("{{SERVER_MODE}}"));
    }

    #[test]
    fn embeds_local_syntax_highlighter_in_web_index() {
        let html = render_web_index(false);
        assert!(html.contains("hljs.highlightAuto"));
        assert!(!html.contains(HIGHLIGHT_JS_MARKER));
    }

    #[test]
    fn embeds_elk_and_defers_node_placement_to_the_browser() {
        let html = render_web_index(false);
        assert!(html.contains("org.eclipse.elk.layered"));
        assert!(html.contains("elk.edgeRouting"));
        assert!(!html.contains(ELK_JS_MARKER));
        assert!(!html.contains(ELK_LICENSE_MARKER));
        assert!(html.contains("Eclipse Public License - v 2.0"));
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

        assert!(output_dir.join("graph.json").is_file());
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
        let html = fs::read_to_string(&path).unwrap();

        assert_eq!(path.file_name().unwrap(), "response.html");
        assert!(html.contains("<svg"));
        assert!(html.contains("id=\"graph-data\""));
        assert!(path.with_file_name("graph.json").exists());
        assert!(path.with_file_name("graph.mmd").exists());
        fs::remove_dir_all(path.parent().unwrap()).unwrap();
    }

    #[test]
    fn writes_positioned_json_for_final_response_history() {
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
        let graph_json = fs::read_to_string(output_dir.join("graph.json")).unwrap();
        let document: serde_json::Value = serde_json::from_str(&graph_json).unwrap();
        let html = fs::read_to_string(html_path).unwrap();

        assert_eq!(document["nodes"].as_array().unwrap().len(), 3);
        assert_eq!(document["edges"].as_array().unwrap().len(), 2);
        assert!(html.contains(&graph_json));
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
