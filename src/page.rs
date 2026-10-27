//! Lays out validated graphs, exports their positioned JSON, and writes the
//! browser page and response history. Mermaid source export lives separately.

use crate::graph_parser::{Graph, GraphDirection};
use crate::step::{Role, Step};
use serde::Serialize;
use std::collections::{HashMap, VecDeque};
use std::fs;
use std::io;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::time::{SystemTime, UNIX_EPOCH};

const TEMPLATE: &str = include_str!("../public/index.html");
const GRAPH_JSON_MARKER: &str = "{{GRAPH_JSON}}";
const ROUTER_WASM_MARKER: &str = "{{ROUTER_WASM_BASE64}}";
const GRAPH_DETAILS_MARKER: &str = "{{GRAPH_DETAILS}}";
const SERVER_MODE_MARKER: &str = "{{SERVER_MODE}}";
const NODE_WIDTH: f64 = 210.0;
const NODE_HEIGHT: f64 = 112.0;
const HORIZONTAL_GAP: f64 = 90.0;
const VERTICAL_GAP: f64 = 80.0;
const MARGIN: f64 = 48.0;
const LANE_GAP: f64 = 72.0;

pub fn write_response_page(thought: Option<&str>, response: &str) -> io::Result<PathBuf> {
    let output_dir = Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("target")
        .join("generated");
    write_response_files(&output_dir, thought, response, None)
}

pub fn write_graph_page(graph: &Graph, graph_source: &str) -> io::Result<PathBuf> {
    let graph_json = serialize_positioned_graph(graph)?;
    let wasm_base64 = router_wasm_base64();
    let html = render_page_html(graph, &graph_json, &wasm_base64, false);
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
    let Some(final_step) = history.last() else {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            "cannot write response history without an LLM response",
        ));
    };
    let graph = crate::graph_file::parse_graph(&final_step.response.content)?;
    let graph_json = serialize_positioned_graph(&graph)?;
    let wasm_base64 = router_wasm_base64();
    let html = render_page_html(&graph, &graph_json, &wasm_base64, false);
    let output_path = output_dir.join("response.html");
    fs::write(&output_path, html)?;
    fs::write(output_dir.join("graph.json"), &graph_json)?;
    crate::mermaid::write_graph_file(output_dir, &graph)?;
    crate::graph_file::write_graph_file(
        output_dir,
        crate::graph_file::normalize_graph_source(&final_step.response.content),
    )?;
    write_response_history_snapshot(output_dir, history, validation_errors)?;
    Ok(output_path)
}

fn write_response_files(
    generated_dir: &Path,
    thought: Option<&str>,
    response: &str,
    history: Option<&[Step]>,
) -> io::Result<PathBuf> {
    let graph = crate::graph_file::parse_graph(response)?;
    let graph_json = serialize_positioned_graph(&graph)?;
    let wasm_base64 = router_wasm_base64();
    let html = render_page_html(&graph, &graph_json, &wasm_base64, false);
    let raw_graph = crate::graph_file::normalize_graph_source(response);

    fs::create_dir_all(generated_dir)?;
    let output_dir = create_timestamped_dir(generated_dir)?;
    let output_path = output_dir.join("response.html");
    fs::write(&output_path, html)?;
    fs::write(output_dir.join("graph.json"), &graph_json)?;
    crate::mermaid::write_graph_file(&output_dir, &graph)?;
    crate::graph_file::write_graph_file(&output_dir, raw_graph)?;
    let text = history.map_or_else(
        || format_attempt(1, None, thought, response, None),
        |history| format_history(history, &[]),
    );
    fs::write(output_dir.join("response.txt"), text)?;
    Ok(output_path)
}

#[derive(Serialize)]
struct PositionedGraph {
    direction: &'static str,
    name: Option<String>,
    description: Option<String>,
    width: f64,
    height: f64,
    nodes: Vec<PositionedNode>,
    edges: Vec<PositionedEdge>,
}

#[derive(Serialize)]
struct PositionedNode {
    id: String,
    name: Option<String>,
    description: Option<String>,
    x: f64,
    y: f64,
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

pub(crate) fn serialize_positioned_graph(graph: &Graph) -> io::Result<String> {
    let (positions, width, height) = layout_graph(graph);
    let document = PositionedGraph {
        direction: match graph.direction {
            GraphDirection::TopDown => "TD",
            GraphDirection::LeftRight => "LR",
        },
        name: graph.name.clone(),
        description: graph.description.clone(),
        width,
        height,
        nodes: graph
            .nodes
            .iter()
            .map(|node| {
                let &(x, y) = positions.get(node.id.as_str()).ok_or_else(|| {
                    io::Error::other(format!("layout did not place graph node '{}'", node.id))
                })?;
                Ok(PositionedNode {
                    id: node.id.clone(),
                    name: node.name.clone(),
                    description: node.description.clone(),
                    x,
                    y,
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
    wasm_base64: &str,
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
        .replace(ROUTER_WASM_MARKER, wasm_base64)
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
    };
    let graph_json =
        serialize_positioned_graph(&graph).expect("an empty graph can always be serialized");
    let wasm_base64 = router_wasm_base64();
    render_page_html(&graph, &graph_json, &wasm_base64, server_mode)
}

fn router_wasm_base64() -> String {
    encode_base64(include_bytes!(concat!(
        env!("OUT_DIR"),
        "/codemap_router.wasm"
    )))
}

fn encode_base64(bytes: &[u8]) -> String {
    const ALPHABET: &[u8; 64] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";
    let mut encoded = String::with_capacity(bytes.len().div_ceil(3) * 4);
    for chunk in bytes.chunks(3) {
        let first = chunk[0];
        let second = *chunk.get(1).unwrap_or(&0);
        let third = *chunk.get(2).unwrap_or(&0);
        encoded.push(ALPHABET[(first >> 2) as usize] as char);
        encoded.push(ALPHABET[(((first & 0b11) << 4) | (second >> 4)) as usize] as char);
        encoded.push(if chunk.len() > 1 {
            ALPHABET[(((second & 0b1111) << 2) | (third >> 6)) as usize] as char
        } else {
            '='
        });
        encoded.push(if chunk.len() > 2 {
            ALPHABET[(third & 0b111111) as usize] as char
        } else {
            '='
        });
    }
    encoded
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
        text.push_str(&escape_html(prompt));
        text.push_str("\n</error-prompt>\n");
    }
    if let Some(thought) = thought {
        text.push_str("<thinking>\n");
        text.push_str(&escape_html(thought));
        text.push_str("\n</thinking>\n");
    }
    text.push_str("<output>\n");
    text.push_str(&escape_html(response));
    text.push_str("\n</output>\n");
    if let Some(error) = validation_error {
        text.push_str("<validation-error>\n");
        text.push_str(&escape_html(error));
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

fn layout_graph(graph: &Graph) -> (HashMap<String, (f64, f64)>, f64, f64) {
    if graph.nodes.is_empty() {
        return (HashMap::new(), 320.0, 200.0);
    }

    let mut bands = HashMap::new();
    let mut discovery_order = Vec::with_capacity(graph.nodes.len());
    let mut queue = VecDeque::new();

    for node in &graph.nodes {
        if bands.contains_key(&node.id) {
            continue;
        }
        let start_band = bands.values().copied().max().map_or(0, |band| band + 2);
        bands.insert(node.id.clone(), start_band);
        discovery_order.push(node.id.clone());
        queue.push_back(node.id.clone());

        while let Some(current) = queue.pop_front() {
            let current_band = bands[&current];
            for edge in &graph.edges {
                let neighbor_and_band = if edge.arrow_start && edge.arrow_end {
                    if edge.from == current {
                        Some((&edge.to, current_band))
                    } else if edge.to == current {
                        Some((&edge.from, current_band))
                    } else {
                        None
                    }
                } else if edge.from == current && edge.arrow_end {
                    Some((&edge.to, current_band + 1))
                } else if edge.to == current {
                    Some((&edge.from, current_band - 1))
                } else {
                    None
                };

                if let Some((neighbor, band)) = neighbor_and_band
                    && !bands.contains_key(neighbor)
                {
                    bands.insert(neighbor.clone(), band);
                    discovery_order.push(neighbor.clone());
                    queue.push_back(neighbor.clone());
                }
            }
        }
    }

    let mut nodes_by_band = HashMap::<i32, Vec<String>>::new();
    for id in &discovery_order {
        let band = bands[id];
        nodes_by_band.entry(band).or_default().push(id.clone());
    }
    for (band, ids) in &mut nodes_by_band {
        let discovered = ids.clone();
        let mut ordered = Vec::with_capacity(ids.len());
        for id in discovered {
            let mut lateral_queue = VecDeque::from([id]);
            while let Some(current) = lateral_queue.pop_front() {
                if ordered.contains(&current) {
                    continue;
                }
                ordered.push(current.clone());
                for edge in &graph.edges {
                    let neighbor = if edge.arrow_start && edge.arrow_end && edge.from == current {
                        Some(&edge.to)
                    } else if edge.arrow_start && edge.arrow_end && edge.to == current {
                        Some(&edge.from)
                    } else {
                        None
                    };
                    if let Some(neighbor) = neighbor
                        && bands.get(neighbor) == Some(band)
                        && ids.contains(neighbor)
                        && !ordered.contains(neighbor)
                    {
                        lateral_queue.push_back(neighbor.clone());
                    }
                }
            }
        }
        *ids = ordered;
    }

    let min_band = *bands.values().min().unwrap_or(&0);
    let max_band = *bands.values().max().unwrap_or(&0);
    let max_lanes = nodes_by_band.values().map(Vec::len).max().unwrap_or(1);
    let mut positions = HashMap::with_capacity(graph.nodes.len());

    for (band, ids) in &nodes_by_band {
        for (lane, id) in ids.iter().enumerate() {
            let (x, y) = match graph.direction {
                GraphDirection::LeftRight => (
                    MARGIN + (band - min_band) as f64 * (NODE_WIDTH + HORIZONTAL_GAP),
                    MARGIN + lane as f64 * (NODE_HEIGHT + LANE_GAP),
                ),
                GraphDirection::TopDown => (
                    MARGIN + lane as f64 * (NODE_WIDTH + LANE_GAP),
                    MARGIN + (band - min_band) as f64 * (NODE_HEIGHT + VERTICAL_GAP),
                ),
            };
            positions.insert(id.clone(), (x, y));
        }
    }

    let (width, height) = match graph.direction {
        GraphDirection::LeftRight => (
            MARGIN * 2.0
                + (max_band - min_band) as f64 * (NODE_WIDTH + HORIZONTAL_GAP)
                + NODE_WIDTH,
            MARGIN * 2.0
                + max_lanes.saturating_sub(1) as f64 * (NODE_HEIGHT + LANE_GAP)
                + NODE_HEIGHT,
        ),
        GraphDirection::TopDown => (
            MARGIN * 2.0
                + max_lanes.saturating_sub(1) as f64 * (NODE_WIDTH + LANE_GAP)
                + NODE_WIDTH,
            MARGIN * 2.0
                + (max_band - min_band) as f64 * (NODE_HEIGHT + VERTICAL_GAP)
                + NODE_HEIGHT,
        ),
    };
    (positions, width.max(320.0), height.max(200.0))
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
    fn encodes_router_wasm_as_base64() {
        assert_eq!(encode_base64(b"f"), "Zg==");
        assert_eq!(encode_base64(b"fo"), "Zm8=");
        assert_eq!(encode_base64(b"foo"), "Zm9v");
    }

    #[test]
    fn web_index_leaves_graph_metadata_empty_for_javascript() {
        let html = render_web_index(false);
        assert!(html.contains("<div class=\"graph-details\" id=\"graph-details\"></div>"));
        assert!(!html.contains("<h1>CodeMap</h1>"));
        assert!(!html.contains("{{GRAPH_DETAILS}}"));
        assert!(html.contains("data-server-mode=\"false\""));
    }

    #[test]
    fn server_web_index_is_marked_for_the_browser() {
        let html = render_web_index(true);
        assert!(html.contains("data-server-mode=\"true\""));
        assert!(!html.contains("{{SERVER_MODE}}"));
    }

    #[test]
    fn escapes_graph_metadata_in_response_page() {
        let graph = parse(
            r#"<graph type="TD" name="A & <B>" desc="Checks <unsafe> & output">
Input["<name>Input</name>"]
</graph>"#,
        )
        .unwrap();

        let graph_json = serialize_positioned_graph(&graph).unwrap();
        let html = render_page_html(&graph, &graph_json, "AA==", false);
        assert!(html.contains("<title>A &amp; &lt;B&gt;</title>"));
        assert!(
            html.contains("<p class=\"graph-description\">Checks &lt;unsafe&gt; &amp; output</p>")
        );
        assert!(graph_json.contains(r#"A & \u003cB>"#));
        assert!(!graph_json.contains("</script>"));
    }

    #[test]
    fn exports_node_positions_and_connection_data_as_json() {
        let graph = parse(GRAPH).unwrap();
        let json = serialize_positioned_graph(&graph).unwrap();
        let document: serde_json::Value = serde_json::from_str(&json).unwrap();

        assert_eq!(document["direction"], "LR");
        assert_eq!(document["name"], "Test graph");
        assert_eq!(document["nodes"].as_array().unwrap().len(), 3);
        assert_eq!(document["nodes"][0]["id"], "Input");
        assert_eq!(document["nodes"][0]["x"], MARGIN);
        assert_eq!(document["nodes"][0]["y"], MARGIN);
        assert_eq!(document["nodes"][0]["width"], NODE_WIDTH);
        assert_eq!(document["edges"].as_array().unwrap().len(), 2);
        assert_eq!(document["edges"][1]["label"], "optional");
        assert_eq!(document["edges"][1]["dotted"], true);
        assert_eq!(document["edges"][1]["arrow_end"], true);
    }

    #[test]
    fn lays_out_children_on_next_row_and_new_parents_on_previous_row() {
        let graph = parse(
            r#"<graph type="TD" name="Layout test" desc="Checks parent and child placement">
Root["<name>Root</name>"]
Child["<name>Child</name>"]
Parent["<name>Parent</name>"]
Root --> Child
Parent --> Root
</graph>
"#,
        )
        .unwrap();
        let (positions, _, _) = layout_graph(&graph);
        let root = positions["Root"];
        let child = positions["Child"];
        let parent = positions["Parent"];

        assert_eq!(root.0, child.0);
        assert_eq!(root.0, parent.0);
        assert!(parent.1 < root.1);
        assert!(root.1 < child.1);
    }

    #[test]
    fn keeps_siblings_on_the_same_band_and_spaces_them_apart() {
        let graph = parse(
            r#"<graph type="LR" name="Sibling layout" desc="Checks sibling placement">
Root["<name>Root</name>"]
First["<name>First</name>"]
Second["<name>Second</name>"]
Root --> First
Root --> Second
</graph>
"#,
        )
        .unwrap();
        let (positions, _, _) = layout_graph(&graph);
        let first = positions["First"];
        let second = positions["Second"];

        assert_eq!(first.0, second.0);
        assert!((first.1 - second.1).abs() >= NODE_HEIGHT + LANE_GAP);
    }

    #[test]
    fn keeps_two_way_neighbors_on_the_same_band_next_to_each_other() {
        let graph = parse(
            r#"<graph type="LR" name="Neighbor layout" desc="Checks two-way neighbor placement">
Root["<name>Root</name>"]
Neighbor["<name>Neighbor</name>"]
Child["<name>Child</name>"]
Root <--> Neighbor
Root --> Child
</graph>
"#,
        )
        .unwrap();
        let (positions, _, _) = layout_graph(&graph);
        let root = positions["Root"];
        let neighbor = positions["Neighbor"];
        let child = positions["Child"];

        assert_eq!(root.0, neighbor.0);
        assert!((root.1 - neighbor.1).abs() >= NODE_HEIGHT + LANE_GAP);
        assert!(child.0 > root.0);
    }

    #[test]
    fn writes_custom_page_raw_graph_page_and_original_response() {
        let unique = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .as_nanos();
        let generated_dir = std::env::temp_dir().join(format!("hello-cli-page-{unique}"));
        let thought = "Connect the three steps.";
        let html_path = write_response_files(&generated_dir, Some(thought), GRAPH, None).unwrap();
        let output_dir = html_path.parent().unwrap();

        assert_eq!(html_path.file_name().unwrap(), "response.html");
        let custom_page = fs::read_to_string(&html_path).unwrap();
        assert!(custom_page.contains("<svg"));
        assert!(custom_page.contains("id=\"graph-data\""));
        assert!(custom_page.contains("JSON.parse(document.getElementById"));
        assert!(custom_page.contains("nodesById.get(edge.from)"));
        assert!(!custom_page.contains("<g class=\"node\">"));
        assert!(custom_page.contains("<title>Test graph</title>"));
        assert!(custom_page.contains("<h1>Test graph</h1>"));
        assert!(custom_page.contains(
            "<p class=\"graph-description\">Shows an input, transformation, and output</p>"
        ));
        assert!(!custom_page.contains("mermaid.initialize"));

        let graph_json = fs::read_to_string(output_dir.join("graph.json")).unwrap();
        let document: serde_json::Value = serde_json::from_str(&graph_json).unwrap();
        assert_eq!(document["nodes"].as_array().unwrap().len(), 3);
        assert_eq!(document["edges"].as_array().unwrap().len(), 2);
        assert!(custom_page.contains(&graph_json));

        let mermaid_file = fs::read_to_string(output_dir.join("graph.mmd")).unwrap();
        assert!(mermaid_file.contains("graph LR"));
        assert!(!output_dir.join("mermaid.html").exists());

        let graph_file = fs::read_to_string(output_dir.join("graph.gr")).unwrap();
        assert_eq!(graph_file, GRAPH.trim());

        let text = fs::read_to_string(output_dir.join("response.txt")).unwrap();
        assert!(text.contains(&format!("<thinking>\n{thought}\n</thinking>")));
        assert!(text.contains(&format!("<output>\n{}\n</output>", escape_html(GRAPH))));

        fs::remove_dir_all(generated_dir).unwrap();
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
        let retry_output = text.find(&escape_html(GRAPH)).unwrap();

        assert!(first_output < correction);
        assert!(validation_error < correction);
        assert!(correction < retry_thought);
        assert!(retry_thought < retry_output);
        assert!(text.contains("Fix the issue on line 4"));
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
        let error = write_response_files(&generated_dir, None, "not a graph", None).unwrap_err();
        assert_eq!(error.kind(), io::ErrorKind::InvalidData);
        assert!(!generated_dir.exists());
    }
}
