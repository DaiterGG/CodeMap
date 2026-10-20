//! Serializes parsed custom graphs as Mermaid `.mmd` source files.

use crate::graph_parser::{Graph, GraphDirection, GraphEdge};
use std::fs;
use std::io;
use std::path::{Path, PathBuf};

pub fn write_graph_file(directory: impl AsRef<Path>, graph: &Graph) -> io::Result<PathBuf> {
    let path = directory.as_ref().join("graph.mmd");
    fs::write(&path, to_source(graph))?;
    Ok(path)
}

pub fn to_source(graph: &Graph) -> String {
    let direction = match graph.direction {
        GraphDirection::TopDown => "TD",
        GraphDirection::LeftRight => "LR",
    };
    let mut source = format!("graph {direction}\n");

    for node in &graph.nodes {
        let mut label = Vec::new();
        if let Some(name) = &node.name {
            label.push(escape_label(name));
        }
        if let Some(description) = &node.description {
            label.push(escape_label(description));
        }
        source.push_str(&format!("{}[\"{}\"]\n", node.id, label.join("<br/>")));
    }

    for edge in &graph.edges {
        source.push_str(&edge.from);
        source.push(' ');
        source.push_str(&connector(edge));
        if !edge.label.is_empty() {
            source.push('|');
            source.push_str(&escape_edge_label(&edge.label));
            source.push('|');
        }
        source.push(' ');
        source.push_str(&edge.to);
        source.push('\n');
    }

    source
}

fn connector(edge: &GraphEdge) -> &'static str {
    match (edge.dotted, edge.arrow_start, edge.arrow_end) {
        (false, true, true) => "<-->",
        (false, false, true) => "-->",
        (false, false, false) => "---",
        (true, true, true) => "<-.->",
        (true, false, true) => "-.->",
        (true, false, false) => "-.-",
        (false, true, false) => "<--",
        (true, true, false) => "<-.-",
    }
}

fn escape_label(value: &str) -> String {
    value
        .replace('&', "#amp;")
        .replace('"', "#quot;")
        .replace('<', "#lt;")
        .replace('>', "#gt;")
        .replace('\n', "<br/>")
}

fn escape_edge_label(value: &str) -> String {
    escape_label(value).replace('|', "#124;")
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::time::{SystemTime, UNIX_EPOCH};

    #[test]
    fn exports_parsed_graph_as_mermaid_file_without_an_html_page() {
        let unique = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .as_nanos();
        let directory = std::env::temp_dir().join(format!("hello-cli-mermaid-{unique}"));
        fs::create_dir_all(&directory).unwrap();
        let graph = crate::graph_parser::parse(
            "graph LR\nInput[\"<name>Input</name><desc>Read & transform</desc>\"]\nOutput[\"<name>Output</name>\"]\nInput -.->|optional| Output",
        )
        .unwrap();

        let path = write_graph_file(&directory, &graph).unwrap();

        assert_eq!(path.file_name().unwrap(), "graph.mmd");
        let source = fs::read_to_string(path).unwrap();
        assert!(source.starts_with("graph LR\n"));
        assert!(source.contains("Input[\"Input<br/>Read #amp; transform\"]"));
        assert!(source.contains("Input -.->|optional| Output"));
        assert!(!directory.join("mermaid.html").exists());

        fs::remove_dir_all(directory).unwrap();
    }
}
