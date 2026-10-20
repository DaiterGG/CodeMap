//! Renders validated graphs as positioned, styled SVG-based HTML and writes
//! custom graph exports and response history. Mermaid source export lives in
//! the dedicated `mermaid` module.

use crate::graph_parser::{Graph, GraphDirection, GraphEdge, GraphNode};
use crate::step::{Role, Step};
use std::collections::{HashMap, VecDeque};
use std::fs;
use std::io;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::time::{SystemTime, UNIX_EPOCH};

const TEMPLATE: &str = include_str!("../public/index.html");
const SVG_MARKER: &str = "{{SVG}}";
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
    let html = TEMPLATE.replace(SVG_MARKER, &render_svg(graph));
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
    let Some(final_step) = history.last() else {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            "cannot write response history without an LLM response",
        ));
    };
    let output_dir = Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("target")
        .join("generated");
    write_response_files(
        &output_dir,
        final_step.response.reasoning.as_deref(),
        &final_step.response.content,
        Some(history),
    )
}

fn write_response_files(
    generated_dir: &Path,
    thought: Option<&str>,
    response: &str,
    history: Option<&[Step]>,
) -> io::Result<PathBuf> {
    let graph = crate::graph_file::parse_graph(response)?;
    let svg = render_svg(&graph);
    let html = TEMPLATE.replace(SVG_MARKER, &svg);
    let raw_graph = crate::graph_file::normalize_graph_source(response);

    fs::create_dir_all(generated_dir)?;
    let output_dir = create_timestamped_dir(generated_dir)?;
    let output_path = output_dir.join("response.html");
    fs::write(&output_path, html)?;
    crate::mermaid::write_graph_file(&output_dir, &graph)?;
    crate::graph_file::write_graph_file(&output_dir, raw_graph)?;
    let text = history.map_or_else(
        || format_attempt(1, None, thought, response),
        format_history,
    );
    fs::write(output_dir.join("response.txt"), text)?;
    Ok(output_path)
}

fn format_history(history: &[Step]) -> String {
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
            )
        })
        .collect()
}

fn format_attempt(
    attempt: usize,
    correction_prompt: Option<&str>,
    thought: Option<&str>,
    response: &str,
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
    text.push_str("\n</output>\n</attempt>\n");
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

fn render_svg(graph: &Graph) -> String {
    let (positions, width, height) = layout_graph(graph);

    let mut svg = format!(
        "<svg xmlns=\"http://www.w3.org/2000/svg\" role=\"img\" aria-label=\"Codebase graph\" width=\"{width}\" height=\"{height}\" viewBox=\"0 0 {width} {height}\"><defs><marker id=\"arrow\" markerWidth=\"10\" markerHeight=\"8\" refX=\"9\" refY=\"4\" orient=\"auto-start-reverse\"><path d=\"M0,0 L10,4 L0,8 z\" fill=\"#57606a\"/></marker></defs>"
    );

    for edge in &graph.edges {
        if let (Some(&(from_x, from_y)), Some(&(to_x, to_y))) = (
            positions.get(edge.from.as_str()),
            positions.get(edge.to.as_str()),
        ) {
            render_edge(&mut svg, edge, from_x, from_y, to_x, to_y);
        }
    }
    for node in &graph.nodes {
        if let Some(&(x, y)) = positions.get(node.id.as_str()) {
            render_node(&mut svg, node, x, y);
        }
    }
    svg.push_str("</svg>");
    svg
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

fn render_edge(svg: &mut String, edge: &GraphEdge, from_x: f64, from_y: f64, to_x: f64, to_y: f64) {
    let from_center_x = from_x + NODE_WIDTH / 2.0;
    let from_center_y = from_y + NODE_HEIGHT / 2.0;
    let to_center_x = to_x + NODE_WIDTH / 2.0;
    let to_center_y = to_y + NODE_HEIGHT / 2.0;
    let (start_x, start_y, end_x, end_y) =
        if (to_center_x - from_center_x).abs() > (to_center_y - from_center_y).abs() {
            let from_side = if to_center_x >= from_center_x {
                from_x + NODE_WIDTH
            } else {
                from_x
            };
            let to_side = if to_center_x >= from_center_x {
                to_x
            } else {
                to_x + NODE_WIDTH
            };
            (from_side, from_center_y, to_side, to_center_y)
        } else {
            let from_side = if to_center_y >= from_center_y {
                from_y + NODE_HEIGHT
            } else {
                from_y
            };
            let to_side = if to_center_y >= from_center_y {
                to_y
            } else {
                to_y + NODE_HEIGHT
            };
            (from_center_x, from_side, to_center_x, to_side)
        };
    let dotted = if edge.dotted { " dotted" } else { "" };
    let marker_start = if edge.arrow_start {
        " marker-start=\"url(#arrow)\""
    } else {
        ""
    };
    let marker_end = if edge.arrow_end {
        " marker-end=\"url(#arrow)\""
    } else {
        ""
    };
    svg.push_str(&format!(
        "<line class=\"edge{dotted}\" x1=\"{start_x}\" y1=\"{start_y}\" x2=\"{end_x}\" y2=\"{end_y}\"{marker_start}{marker_end}/>"
    ));
    if !edge.label.is_empty() {
        let label_x = (start_x + end_x) / 2.0;
        let label_y = (start_y + end_y) / 2.0 - 5.0;
        svg.push_str(&format!(
            "<text class=\"edge-label\" x=\"{label_x}\" y=\"{label_y}\" text-anchor=\"middle\">{}</text>",
            escape_html(&edge.label)
        ));
    }
}

fn render_node(svg: &mut String, node: &GraphNode, x: f64, y: f64) {
    svg.push_str(&format!(
        "<g class=\"node\"><rect x=\"{x}\" y=\"{y}\" width=\"{NODE_WIDTH}\" height=\"{NODE_HEIGHT}\"/>"
    ));
    let title = node.name.as_deref().unwrap_or_default();
    let description = node.description.as_deref().unwrap_or_default();

    if !title.is_empty() {
        render_text_lines(
            svg,
            "node-title",
            title,
            x + NODE_WIDTH / 2.0,
            y + 32.0,
            18.0,
        );
    }
    if !description.is_empty() {
        let title_lines = wrapped_lines(title, 24, 2).len();
        let start_y = if title.is_empty() {
            y + 50.0
        } else {
            y + 42.0 + title_lines as f64 * 17.0
        };
        render_text_lines(
            svg,
            "node-description",
            description,
            x + NODE_WIDTH / 2.0,
            start_y,
            15.0,
        );
    }
    svg.push_str("</g>");
}

fn render_text_lines(svg: &mut String, class: &str, text: &str, x: f64, y: f64, line_height: f64) {
    for (index, line) in wrapped_lines(text, 24, 3).iter().enumerate() {
        let line_y = y + index as f64 * line_height;
        svg.push_str(&format!(
            "<text class=\"{class}\" x=\"{x}\" y=\"{line_y}\" text-anchor=\"middle\">{}</text>",
            escape_html(line)
        ));
    }
}

fn wrapped_lines(text: &str, max_chars: usize, max_lines: usize) -> Vec<String> {
    let mut lines = Vec::new();
    let mut current = String::new();
    for word in text.split_whitespace() {
        if !current.is_empty() && current.chars().count() + 1 + word.chars().count() > max_chars {
            lines.push(std::mem::take(&mut current));
            if lines.len() == max_lines {
                break;
            }
        }
        if !current.is_empty() {
            current.push(' ');
        }
        current.push_str(word);
    }
    if !current.is_empty() && lines.len() < max_lines {
        lines.push(current);
    }
    if lines.is_empty() {
        lines.push(String::new());
    }
    let rendered_chars = lines.iter().map(|line| line.chars().count()).sum::<usize>();
    if lines.len() == max_lines && text.chars().count() > rendered_chars {
        if let Some(last) = lines.last_mut() {
            last.push_str("...");
        }
    }
    lines
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

    const GRAPH: &str = r#"graph LR
Input["<name>Input</name><desc>Read source</desc>"]
Process["<name>Process</name><desc>Transform source</desc>"]
Output["<desc>Write output</desc>"]
Input --> Process
Process -.->|optional| Output
"#;

    #[test]
    fn escapes_html_in_text() {
        assert_eq!(
            escape_html("<script a=\"b\">Tom & Jerry's</script>"),
            "&lt;script a=&quot;b&quot;&gt;Tom &amp; Jerry&#39;s&lt;/script&gt;"
        );
    }

    #[test]
    fn renders_svg_boxes_and_connections_from_parsed_graph() {
        let svg = render_svg(&parse(GRAPH).unwrap());
        assert!(svg.starts_with("<svg"));
        assert_eq!(svg.matches("<g class=\"node\">").count(), 3);
        assert_eq!(svg.matches("<line class=\"edge").count(), 2);
        assert!(svg.contains("class=\"edge dotted\""));
        assert!(svg.contains("Input"));
        assert!(svg.contains("Read source"));
        assert!(svg.contains(">optional</text>"));
    }

    #[test]
    fn renders_start_and_end_arrowheads_and_dotted_lines() {
        let cases = [
            (false, true, false, "marker-end=\"url(#arrow)\""),
            (true, false, false, "marker-start=\"url(#arrow)\""),
            (true, true, false, "marker-start=\"url(#arrow)\""),
            (false, false, true, "class=\"edge dotted\""),
            (false, false, false, "class=\"edge\""),
        ];

        for (arrow_start, arrow_end, dotted, expected) in cases {
            let edge = GraphEdge {
                from: "A".to_owned(),
                to: "B".to_owned(),
                label: String::new(),
                dotted,
                arrow_start,
                arrow_end,
            };
            let mut svg = String::new();
            render_edge(&mut svg, &edge, 0.0, 0.0, 300.0, 0.0);
            assert!(svg.contains(expected), "{svg}");
            assert_eq!(
                svg.contains("marker-end=\"url(#arrow)\""),
                arrow_end,
                "{svg}"
            );
            assert_eq!(
                svg.contains("marker-start=\"url(#arrow)\""),
                arrow_start,
                "{svg}"
            );
        }
    }

    #[test]
    fn lays_out_children_on_next_row_and_new_parents_on_previous_row() {
        let graph = parse(
            r#"graph TD
Root["<name>Root</name>"]
Child["<name>Child</name>"]
Parent["<name>Parent</name>"]
Root --> Child
Parent --> Root
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
            r#"graph LR
Root["<name>Root</name>"]
First["<name>First</name>"]
Second["<name>Second</name>"]
Root --> First
Root --> Second
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
            r#"graph LR
Root["<name>Root</name>"]
Neighbor["<name>Neighbor</name>"]
Child["<name>Child</name>"]
Root <--> Neighbor
Root --> Child
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
        assert!(custom_page.contains("class=\"node\""));
        assert!(!custom_page.contains("mermaid.initialize"));

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
        assert!(html.contains("Input"));
        assert!(path.with_file_name("graph.mmd").exists());
        fs::remove_dir_all(path.parent().unwrap()).unwrap();
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

        let text = format_history(&history);
        let first_output = text.find("first graph attempt").unwrap();
        let correction = text.find("<error-prompt>").unwrap();
        let retry_thought = text.find("Retry thought").unwrap();
        let retry_output = text.find(&escape_html(GRAPH)).unwrap();

        assert!(first_output < correction);
        assert!(correction < retry_thought);
        assert!(retry_thought < retry_output);
        assert!(text.contains("Fix the issue on line 4"));
        assert_eq!(text.matches("<attempt number=").count(), 2);
        assert!(!text.contains("initial graph request"));
        assert!(!text.contains("source context"));
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
