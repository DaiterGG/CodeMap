//! Reads and writes the application's custom graph source format (`.gr`).

use crate::graph_parser::{Graph, parse};
use std::fs;
use std::io;
use std::path::Path;

pub fn read_graph_file(path: impl AsRef<Path>) -> io::Result<(Graph, String)> {
    let path = path.as_ref();
    let source = fs::read_to_string(path).map_err(|error| {
        io::Error::new(
            error.kind(),
            format!("could not read graph file {}: {error}", path.display()),
        )
    })?;
    let source = normalize_graph_source(&source);
    let graph = parse_graph(&source)?;
    Ok((graph, source.to_owned()))
}

pub fn parse_graph(source: &str) -> io::Result<Graph> {
    parse(source)
        .map_err(|error| io::Error::new(io::ErrorKind::InvalidData, error.correction_details()))
}

pub fn write_graph_file(directory: impl AsRef<Path>, source: &str) -> io::Result<()> {
    fs::write(
        directory.as_ref().join("graph.gr"),
        normalize_graph_source(source),
    )
}

pub fn normalize_graph_source(source: &str) -> &str {
    let mut source = source.trim();
    if source.starts_with("```") {
        source = match source.find('\n') {
            Some(newline) => &source[newline + 1..],
            None => "",
        };
    }
    source = source.trim();
    source.strip_suffix("```").unwrap_or(source).trim()
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::time::{SystemTime, UNIX_EPOCH};

    const GRAPH: &str = "graph LR\nInput[\"<name>Input</name>\"]\nOutput[\"<name>Output</name>\"]\nInput --> Output\n";

    #[test]
    fn reads_and_parses_custom_graph_file() {
        let unique = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .as_nanos();
        let path = std::env::temp_dir().join(format!("hello-cli-graph-{unique}.gr"));
        fs::write(&path, GRAPH).unwrap();

        let (graph, source) = read_graph_file(&path).unwrap();
        assert_eq!(graph.nodes.len(), 2);
        assert_eq!(source, GRAPH.trim());

        fs::remove_file(path).unwrap();
    }

    #[test]
    fn writes_custom_graph_source_with_gr_extension() {
        let unique = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .as_nanos();
        let directory = std::env::temp_dir().join(format!("hello-cli-graph-save-{unique}"));
        fs::create_dir_all(&directory).unwrap();
        write_graph_file(&directory, &format!("```graph\n{GRAPH}```")).unwrap();

        assert_eq!(
            fs::read_to_string(directory.join("graph.gr")).unwrap(),
            GRAPH.trim()
        );

        fs::remove_dir_all(directory).unwrap();
    }
}
