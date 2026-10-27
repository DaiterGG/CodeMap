//! Parses and validates the program's custom graph syntax, reporting syntax and
//! redundant-connection diagnostics used to request LLM corrections.

use std::collections::HashSet;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum DiagnosticKind {
    Syntax,
    RedundantConnection,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct GraphDiagnostic {
    pub kind: DiagnosticKind,
    pub line_number: usize,
    pub line: String,
    pub explanation: String,
    pub expected: String,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum GraphDirection {
    TopDown,
    LeftRight,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct GraphNode {
    pub id: String,
    pub name: Option<String>,
    pub description: Option<String>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct GraphEdge {
    pub from: String,
    pub to: String,
    pub label: String,
    pub dotted: bool,
    pub arrow_start: bool,
    pub arrow_end: bool,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Graph {
    pub direction: GraphDirection,
    pub name: Option<String>,
    pub description: Option<String>,
    pub nodes: Vec<GraphNode>,
    pub edges: Vec<GraphEdge>,
}

impl GraphDiagnostic {
    pub fn correction_details(&self) -> String {
        format!(
            "Detected issue on line {}: {}\nExpected: {}\nProblematic line: {}",
            self.line_number, self.explanation, self.expected, self.line
        )
    }
}

#[derive(Clone, Debug, PartialEq, Eq, Hash)]
struct Connection {
    from: String,
    to: String,
    operator: String,
    label: String,
}

#[derive(Clone, Debug, PartialEq, Eq)]
enum Token {
    Node {
        id: String,
        declaration: Option<Result<NodeLabel, String>>,
    },
    Operator {
        syntax: String,
        label: String,
    },
}

#[derive(Clone, Debug, PartialEq, Eq)]
struct NodeLabel {
    name: Option<String>,
    description: Option<String>,
}

#[derive(Clone, Debug)]
struct ParsedConnection {
    connection: Connection,
    line_number: usize,
}

pub fn validate(output: &str) -> Result<(), GraphDiagnostic> {
    parse(output).map(|_| ())
}

pub fn parse(output: &str) -> Result<Graph, GraphDiagnostic> {
    let lines = output.lines().collect::<Vec<_>>();
    let mut header_seen = false;
    let mut wrapper_seen = false;
    let mut wrapper_closed = false;
    let mut direction = GraphDirection::TopDown;
    let mut graph_name = None;
    let mut graph_description = None;
    let mut declarations = HashSet::new();
    let mut nodes = Vec::new();
    let mut connections = HashSet::new();
    let mut parsed_connections = Vec::new();
    let mut graph_edges = Vec::new();

    for (line_index, raw_line) in lines.iter().enumerate() {
        let line_number = line_index + 1;
        let line = raw_line.trim();
        if line.is_empty() || line.starts_with("%%") || is_fence(line) {
            continue;
        }

        if line.starts_with("<graph") {
            if header_seen || wrapper_seen {
                return Err(diagnostic(
                    DiagnosticKind::Syntax,
                    line_number,
                    raw_line,
                    "A graph wrapper appears after graph content or another graph header.",
                    "Use exactly one `<graph type=\"TD|LR\" ...>` wrapper around the graph.",
                ));
            }
            let (parsed_direction, name, description) =
                parse_graph_wrapper(line).map_err(|explanation| {
                    diagnostic(
                        DiagnosticKind::Syntax,
                        line_number,
                        raw_line,
                        &explanation,
                        "Open the graph with `<graph type=\"TD|LR\" name=\"...\" desc=\"...\">`; name and desc are optional.",
                    )
                })?;
            header_seen = true;
            wrapper_seen = true;
            direction = parsed_direction;
            graph_name = name;
            graph_description = description;
            continue;
        }

        if line == "</graph>" {
            if !wrapper_seen || wrapper_closed {
                return Err(diagnostic(
                    DiagnosticKind::Syntax,
                    line_number,
                    raw_line,
                    "A graph closing tag has no matching open graph wrapper.",
                    "Close the single graph wrapper with `</graph>`.",
                ));
            }
            wrapper_closed = true;
            continue;
        }

        if wrapper_closed {
            return Err(diagnostic(
                DiagnosticKind::Syntax,
                line_number,
                raw_line,
                "Graph content appears after the closing graph tag.",
                "Place all node declarations and connections inside `<graph>...</graph>`.",
            ));
        }

        if !header_seen {
            return Err(diagnostic(
                DiagnosticKind::Syntax,
                line_number,
                raw_line,
                "A graph statement appears outside the required graph wrapper.",
                "Wrap the graph in `<graph type=\"TD|LR\" name=\"...\" desc=\"...\">` and `</graph>`.",
            ));
        }

        let tokens = tokenize_line(line).map_err(|explanation| {
            diagnostic(
                DiagnosticKind::Syntax,
                line_number,
                raw_line,
                &explanation,
                "Close every node label with its matching bracket and close any quoted label text.",
            )
        })?;

        for token in &tokens {
            if let Token::Node { id, declaration } = token {
                if !is_valid_node_id(id) {
                    return Err(diagnostic(
                        DiagnosticKind::Syntax,
                        line_number,
                        raw_line,
                        &format!("Node ID `{id}` is not a valid identifier."),
                        "Use a unique UpperCamelCase ID containing only letters and digits; do not use the reserved ID `end`.",
                    ));
                }
                if let Some(label_result) = declaration {
                    let label = label_result.as_ref().map_err(|explanation| {
                        diagnostic(
                            DiagnosticKind::Syntax,
                            line_number,
                            raw_line,
                            explanation,
                            "Include a complete `<name>...</name>` tag, a complete `<desc>...</desc>` tag, or both in that order.",
                        )
                    })?;
                    if !declarations.insert(id.clone()) {
                        return Err(diagnostic(
                            DiagnosticKind::Syntax,
                            line_number,
                            raw_line,
                            &format!("Node `{id}` is declared more than once."),
                            "Declare each node ID once, then refer to it by ID in connections.",
                        ));
                    }
                    nodes.push(GraphNode {
                        id: id.clone(),
                        name: label.name.clone(),
                        description: label.description.clone(),
                    });
                }
            }
        }

        let mut previous_node: Option<&str> = None;
        let mut previous_operator: Option<(&str, &str)> = None;
        let mut line_edges = Vec::new();
        for token in &tokens {
            match token {
                Token::Node { id, .. } => {
                    if let Some((syntax, label)) = previous_operator {
                        let from =
                            previous_node.expect("operator validation requires a source node");
                        let connection = canonical_connection(from, id, syntax, label);
                        if !connections.insert(connection.clone()) {
                            return Err(diagnostic(
                                DiagnosticKind::RedundantConnection,
                                line_number,
                                raw_line,
                                "This connection duplicates an earlier connection with the same endpoints, direction, connector, and label.",
                                "Keep only one copy of each identical connection.",
                            ));
                        }
                        parsed_connections.push(ParsedConnection {
                            connection,
                            line_number,
                        });
                        let (edge_from, edge_to) = if matches!(syntax, "<--" | "<-.-") {
                            (id.as_str(), from)
                        } else {
                            (from, id.as_str())
                        };
                        line_edges.push(GraphEdge {
                            from: edge_from.to_owned(),
                            to: edge_to.to_owned(),
                            label: label.to_owned(),
                            dotted: syntax.contains('.'),
                            arrow_start: matches!(syntax, "<-->" | "<-.->"),
                            arrow_end: !matches!(syntax, "---" | "-.-"),
                        });
                        previous_operator = None;
                        previous_node = Some(id);
                    } else {
                        if previous_node.is_some() {
                            return Err(diagnostic(
                                DiagnosticKind::Syntax,
                                line_number,
                                raw_line,
                                "Two node IDs appear consecutively without a connection connector.",
                                "Separate node declarations onto different lines, or add one supported connector between the source and target IDs.",
                            ));
                        }
                        previous_node = Some(id);
                    }
                }
                Token::Operator { syntax, label } => {
                    if previous_node.is_none() || previous_operator.is_some() {
                        return Err(diagnostic(
                            DiagnosticKind::Syntax,
                            line_number,
                            raw_line,
                            "A connection connector is missing a source node or follows another connector.",
                            "Write each connection as `SourceID --> TargetID`, with exactly one source and target node around the connector.",
                        ));
                    }
                    previous_operator = Some((syntax, label));
                }
            }
        }
        graph_edges.extend(line_edges);
        if previous_operator.is_some() {
            return Err(diagnostic(
                DiagnosticKind::Syntax,
                line_number,
                raw_line,
                "A connection ends with a connector but has no target node.",
                "Add a target node ID after the connection connector.",
            ));
        }
    }

    if !header_seen {
        return Err(diagnostic(
            DiagnosticKind::Syntax,
            1,
            lines.first().copied().unwrap_or(""),
            "The graph wrapper is missing.",
            "Wrap the graph in `<graph type=\"TD|LR\" name=\"...\" desc=\"...\">` and `</graph>`.",
        ));
    }

    if wrapper_seen && !wrapper_closed {
        return Err(diagnostic(
            DiagnosticKind::Syntax,
            lines.len(),
            lines.last().copied().unwrap_or(""),
            "The graph wrapper is missing its closing tag.",
            "Close the graph with `</graph>`.",
        ));
    }

    if declarations.is_empty() {
        return Err(diagnostic(
            DiagnosticKind::Syntax,
            1,
            lines.first().copied().unwrap_or(""),
            "No labeled node declarations were found.",
            "Declare at least one node with a label containing a complete `<name>...</name>` tag, a complete `<desc>...</desc>` tag, or both.",
        ));
    }

    for parsed in parsed_connections {
        for id in [&parsed.connection.from, &parsed.connection.to] {
            if !declarations.contains(id) {
                return Err(diagnostic(
                    DiagnosticKind::Syntax,
                    parsed.line_number,
                    lines
                        .get(parsed.line_number.saturating_sub(1))
                        .copied()
                        .unwrap_or(""),
                    &format!(
                        "Connection references `{id}`, but that node has no labeled declaration."
                    ),
                    "Declare every referenced node separately with a `<name>`, a `<desc>`, or both in its label.",
                ));
            }
        }
    }

    Ok(Graph {
        direction,
        name: graph_name,
        description: graph_description,
        nodes,
        edges: graph_edges,
    })
}

fn parse_graph_wrapper(
    line: &str,
) -> Result<(GraphDirection, Option<String>, Option<String>), String> {
    let attributes = line
        .strip_prefix("<graph")
        .and_then(|line| line.strip_suffix('>'))
        .ok_or_else(|| "The graph opening tag is malformed.".to_owned())?;
    let mut attributes = attributes.trim();
    let mut direction = None;
    let mut name = None;
    let mut description = None;

    while !attributes.is_empty() {
        let name_end = attributes
            .find(|character: char| character == '=' || character.is_whitespace())
            .unwrap_or(attributes.len());
        if name_end == 0 {
            return Err("The graph opening tag contains a malformed attribute.".to_owned());
        }
        let attribute_name = &attributes[..name_end];
        attributes = attributes[name_end..].trim_start();
        let Some(value) = attributes.strip_prefix('=') else {
            return Err(format!(
                "Graph attribute `{attribute_name}` is missing `=`."
            ));
        };
        attributes = value.trim_start();
        let Some(value) = attributes.strip_prefix('"') else {
            return Err(format!(
                "Graph attribute `{attribute_name}` must use a double-quoted value."
            ));
        };
        let Some(value_end) = value.find('"') else {
            return Err(format!(
                "Graph attribute `{attribute_name}` has no closing quote."
            ));
        };
        let remaining_attributes = value[value_end + 1..].trim_start();
        let value = value[..value_end].to_owned();
        attributes = remaining_attributes;

        let target = match attribute_name {
            "type" => &mut direction,
            "name" => &mut name,
            "desc" => &mut description,
            _ => {
                return Err(format!(
                    "Graph attribute `{attribute_name}` is not supported."
                ));
            }
        };
        if target.replace(value).is_some() {
            return Err(format!(
                "Graph attribute `{attribute_name}` appears more than once."
            ));
        }
    }

    let direction = match direction.as_deref() {
        Some("TD") => GraphDirection::TopDown,
        Some("LR") => GraphDirection::LeftRight,
        Some(_) => return Err("Graph type must be `TD` or `LR`.".to_owned()),
        None => return Err("Graph opening tag requires a `type` attribute.".to_owned()),
    };
    Ok((direction, name, description))
}

fn diagnostic(
    kind: DiagnosticKind,
    line_number: usize,
    line: &str,
    explanation: &str,
    expected: &str,
) -> GraphDiagnostic {
    GraphDiagnostic {
        kind,
        line_number,
        line: line.to_owned(),
        explanation: explanation.to_owned(),
        expected: expected.to_owned(),
    }
}

fn is_fence(line: &str) -> bool {
    line.starts_with("```")
}

fn tokenize_line(line: &str) -> Result<Vec<Token>, String> {
    let bytes = line.as_bytes();
    let mut tokens = Vec::new();
    let mut index = 0;

    while index < bytes.len() {
        if let Some((operator, length)) = read_operator(&bytes[index..]) {
            let syntax = operator.to_owned();
            index += length;
            let mut label = String::new();
            if bytes.get(index) == Some(&b'|') {
                let label_start = index + 1;
                index += 1;
                while index < bytes.len() && bytes[index] != b'|' {
                    index += 1;
                }
                if index == bytes.len() {
                    return Err("A connection label opens with `|` but is not closed.".to_owned());
                }
                label = String::from_utf8_lossy(&bytes[label_start..index])
                    .trim()
                    .to_owned();
                index += 1;
            }
            tokens.push(Token::Operator { syntax, label });
            continue;
        }

        if is_identifier_start(bytes[index]) {
            let start = index;
            index += 1;
            while index < bytes.len() && is_identifier_continue(bytes[index]) {
                index += 1;
            }
            let id = line[start..index].to_owned();
            let mut lookahead = index;
            while lookahead < bytes.len() && bytes[lookahead].is_ascii_whitespace() {
                lookahead += 1;
            }

            let declaration = if let Some(&opening @ (b'[' | b'(' | b'{')) = bytes.get(lookahead) {
                match skip_balanced_label(bytes, lookahead, opening) {
                    Some(end) => {
                        let label = &line[lookahead + 1..end - 1];
                        index = end;
                        Some(parse_node_label(label))
                    }
                    None => {
                        return Err(format!(
                            "Node `{id}` has an unclosed label or an unclosed quote."
                        ));
                    }
                }
            } else {
                None
            };
            tokens.push(Token::Node { id, declaration });
            continue;
        }
        index += 1;
    }
    Ok(tokens)
}

fn parse_node_label(label: &str) -> Result<NodeLabel, String> {
    let name_open = label.find("<name>");
    let name_close = label.find("</name>");
    let desc_open = label.find("<desc>");
    let desc_close = label.find("</desc>");
    match (name_open, name_close, desc_open, desc_close) {
        (None, None, None, None) => Err(
            "The node label must contain a complete `<name>...</name>` tag, a complete `<desc>...</desc>` tag, or both."
                .to_owned(),
        ),
        (Some(no), Some(nc), None, None) if no < nc => Ok(NodeLabel {
            name: Some(label[no + 6..nc].trim().to_owned()),
            description: None,
        }),
        (None, None, Some(do_), Some(dc)) if do_ < dc => Ok(NodeLabel {
            name: None,
            description: Some(label[do_ + 6..dc].trim().to_owned()),
        }),
        (Some(no), Some(nc), Some(do_), Some(dc)) if no < nc && nc < do_ && do_ < dc => {
            Ok(NodeLabel {
                name: Some(label[no + 6..nc].trim().to_owned()),
                description: Some(label[do_ + 6..dc].trim().to_owned()),
            })
        }
        _ => Err(
            "The node label has an incomplete name/description tag or its name and description tags are out of order."
                .to_owned(),
        ),
    }
}

fn read_operator(bytes: &[u8]) -> Option<(&'static str, usize)> {
    [
        ("<-.->", 5),
        ("<-->", 4),
        ("<--", 3),
        ("<-.-", 4),
        ("-.->", 4),
        ("-->", 3),
        ("---", 3),
        ("-.-", 3),
    ]
    .into_iter()
    .find_map(|(operator, length)| {
        bytes
            .starts_with(operator.as_bytes())
            .then_some((operator, length))
    })
}

fn skip_balanced_label(bytes: &[u8], start: usize, opening: u8) -> Option<usize> {
    let closing = match opening {
        b'[' => b']',
        b'(' => b')',
        _ => b'}',
    };
    let mut depth = 0;
    let mut quoted = false;
    let mut index = start;
    while index < bytes.len() {
        match bytes[index] {
            b'"' => quoted = !quoted,
            byte if !quoted && byte == opening => depth += 1,
            byte if !quoted && byte == closing => {
                depth -= 1;
                if depth == 0 {
                    return Some(index + 1);
                }
            }
            _ => {}
        }
        index += 1;
    }
    None
}

fn is_identifier_start(byte: u8) -> bool {
    byte.is_ascii_alphabetic() || byte == b'_'
}

fn is_identifier_continue(byte: u8) -> bool {
    byte.is_ascii_alphanumeric() || byte == b'_'
}

fn is_valid_node_id(id: &str) -> bool {
    id.as_bytes().first().is_some_and(u8::is_ascii_uppercase)
        && id.bytes().all(|byte| byte.is_ascii_alphanumeric())
        && !id.eq_ignore_ascii_case("end")
}

fn canonical_connection(from: &str, to: &str, operator: &str, label: &str) -> Connection {
    let (from, to, operator) = match operator {
        "<--" | "<-.-" => (to, from, if operator == "<--" { "-->" } else { "-.->" }),
        "<-->" | "<-.->" | "---" | "-.-" if from > to => (to, from, operator),
        _ => (from, to, operator),
    };
    Connection {
        from: from.to_owned(),
        to: to.to_owned(),
        operator: operator.to_owned(),
        label: label.to_owned(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const NODE_DECLARATIONS: &str = r#"Alpha["<name>Alpha</name><desc>First node</desc>"]
Beta["<name>Beta</name><desc>Second node</desc>"]
"#;

    fn wrap_graph(direction: &str, body: &str) -> String {
        format!(
            "<graph type=\"{direction}\" name=\"Test graph\" desc=\"Parser test\">\n{body}\n</graph>"
        )
    }

    #[test]
    fn rejects_legacy_mermaid_headers() {
        let error = validate("graph TD\nAlpha[\"<name>Alpha</name>\"]").unwrap_err();
        assert!(
            error
                .explanation
                .contains("outside the required graph wrapper")
        );
    }

    #[test]
    fn detects_duplicate_connections_when_nodes_are_declared_separately() {
        let graph = wrap_graph(
            "TD",
            &format!("Alpha --> Beta\nAlpha --> Beta\n{NODE_DECLARATIONS}"),
        );
        let error = validate(&graph).unwrap_err();
        assert_eq!(error.kind, DiagnosticKind::RedundantConnection);
        assert_eq!(error.line_number, 3);
        assert!(error.explanation.contains("duplicates"));
    }

    #[test]
    fn keeps_distinct_direction_connector_and_label_relationships() {
        let graph = wrap_graph(
            "TD",
            &format!(
                "{NODE_DECLARATIONS}\nAlpha --> Beta\nBeta --> Alpha\nAlpha -.-> Beta\nAlpha -->|calls| Beta\n"
            ),
        );
        assert_eq!(validate(&graph), Ok(()));
    }

    #[test]
    fn parses_supported_connectors_with_their_direction_and_style() {
        let cases = [
            ("<-->", "Alpha", "Beta", false, true, true),
            ("-->", "Alpha", "Beta", false, false, true),
            ("<--", "Beta", "Alpha", false, false, true),
            ("-.->", "Alpha", "Beta", true, false, true),
            ("<-.-", "Beta", "Alpha", true, false, true),
            ("<-.->", "Alpha", "Beta", true, true, true),
            ("---", "Alpha", "Beta", false, false, false),
            ("-.-", "Alpha", "Beta", true, false, false),
        ];

        for (operator, from, to, dotted, arrow_start, arrow_end) in cases {
            let graph = wrap_graph(
                "LR",
                &format!("{NODE_DECLARATIONS}\nAlpha {operator} Beta\n"),
            );
            let parsed = parse(&graph).unwrap();
            let edge = &parsed.edges[0];
            assert_eq!(edge.from, from, "connector {operator}");
            assert_eq!(edge.to, to, "connector {operator}");
            assert_eq!(edge.dotted, dotted, "connector {operator}");
            assert_eq!(edge.arrow_start, arrow_start, "connector {operator}");
            assert_eq!(edge.arrow_end, arrow_end, "connector {operator}");
        }
    }

    #[test]
    fn accepts_connections_before_separate_node_declarations() {
        let graph = wrap_graph("LR", &format!("Alpha --> Beta\n{NODE_DECLARATIONS}"));
        assert_eq!(validate(&graph), Ok(()));
    }

    #[test]
    fn identifies_header_problems_with_a_line_and_expectation() {
        let error = validate("Alpha --> Beta").unwrap_err();
        assert_eq!(error.line_number, 1);
        assert!(
            error
                .explanation
                .contains("outside the required graph wrapper")
        );
        assert!(error.expected.contains("<graph type="));
        assert!(
            error
                .correction_details()
                .contains("Problematic line: Alpha --> Beta")
        );
    }

    #[test]
    fn identifies_unclosed_node_labels_and_quotes() {
        for graph in [
            "<graph type=\"TD\">\nAlpha[\"<name>A</name><desc>Node</desc>\"",
            "<graph type=\"TD\">\nAlpha[\"<name>A</name><desc>Node</desc>]",
        ] {
            let error = validate(graph).unwrap_err();
            assert_eq!(error.line_number, 2);
            assert!(error.explanation.contains("unclosed"));
        }
    }

    #[test]
    fn identifies_missing_or_misordered_node_markers() {
        let error = validate("<graph type=\"TD\">\nAlpha[\"<desc>Node</desc><name>A</name>\"]")
            .unwrap_err();
        assert!(error.explanation.contains("tag"));
        assert!(error.expected.contains("<name>"));
    }

    #[test]
    fn accepts_nodes_with_only_name_or_only_description() {
        let graph = r#"<graph type="LR" name="Test graph" desc="Parser test">
MainLoop["<name>Main loop</name>"]
WriteOutput["<desc>Writes output to file</desc>"]
MainLoop --> WriteOutput
</graph>
"#;
        assert_eq!(validate(graph), Ok(()));
    }

    #[test]
    fn accepts_graph_wrapper_metadata_and_inline_node_declarations() {
        let graph = r#"<graph type="LR" name="Custom data processor layer" desc="Checks data against groundtruth">
Input["<name>Input</name><desc>Parse input</desc>"] --> Processor["<name>Processor</name><desc>Run calculations</desc>"]
Processor --> Output["<name>Output</name>"]
</graph>"#;

        let parsed = parse(graph).unwrap();
        assert_eq!(parsed.direction, GraphDirection::LeftRight);
        assert_eq!(parsed.name.as_deref(), Some("Custom data processor layer"));
        assert_eq!(
            parsed.description.as_deref(),
            Some("Checks data against groundtruth")
        );
        assert_eq!(parsed.nodes.len(), 3);
        assert_eq!(parsed.edges.len(), 2);
    }

    #[test]
    fn requires_graph_wrapper_to_be_closed() {
        let error = validate(
            r#"<graph type="TD">
Input["<name>Input</name>"]"#,
        )
        .unwrap_err();
        assert!(error.explanation.contains("missing its closing tag"));
    }

    #[test]
    fn identifies_invalid_node_ids_and_reserved_word() {
        for id in ["lowercase", "Bad_ID", "end"] {
            let graph = wrap_graph("TD", &format!("{id}[\"<name>N</name><desc>Node</desc>\"]"));
            let error = validate(&graph).unwrap_err();
            assert!(error.explanation.contains("Node ID"));
            assert!(error.expected.contains("UpperCamelCase"));
        }
    }

    #[test]
    fn identifies_dangling_connectors() {
        for graph in [
            wrap_graph("TD", &format!("{NODE_DECLARATIONS}\nAlpha -->\n")),
            wrap_graph("TD", &format!("{NODE_DECLARATIONS}\n--> Beta\n")),
        ] {
            let error = validate(&graph).unwrap_err();
            assert_eq!(error.kind, DiagnosticKind::Syntax);
            assert!(
                error.expected.contains("source and target")
                    || error.expected.contains("target node")
            );
        }
    }

    #[test]
    fn identifies_unrecognized_connection_syntax() {
        let graph = wrap_graph("TD", &format!("{NODE_DECLARATIONS}\nAlpha ==> Beta\n"));
        let error = validate(&graph).unwrap_err();
        assert_eq!(error.line_number, 5);
        assert!(error.explanation.contains("Two node IDs"));
        assert!(error.line.contains("==>"));
    }

    #[test]
    fn reports_connections_to_nodes_without_declarations() {
        let graph = r#"<graph type="TD">
Alpha --> Beta
Alpha["<name>A</name><desc>Node</desc>"]
</graph>"#;
        let error = validate(graph).unwrap_err();
        assert!(error.explanation.contains("Beta"));
        assert!(error.expected.contains("Declare every referenced node"));
    }

    #[test]
    fn accepts_fenced_output() {
        let graph = format!(
            "```text\n{}\n```",
            wrap_graph("TD", &format!("{NODE_DECLARATIONS}Alpha --> Beta\n"))
        );
        assert_eq!(validate(&graph), Ok(()));
    }
}
