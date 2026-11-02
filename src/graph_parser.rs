//! Parses and validates the program's custom graph syntax, reporting syntax and
//! redundant-connection diagnostics used to request LLM corrections.

use crate::files::SourceFile;
use serde::{Deserialize, Serialize};
use std::collections::{HashMap, HashSet};

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum DiagnosticKind {
    Syntax,
    RedundantConnection,
    SourceReference,
    PartialReference,
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
    pub sources: Vec<SourceReference>,
}

#[derive(Clone, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct SourceReference {
    pub path: String,
    pub start: Option<usize>,
    pub end: Option<usize>,
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
    pub partials: Vec<GraphPartial>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct GraphPartial {
    pub id: String,
    pub content: String,
    pub line_number: usize,
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
    Partial {
        id: String,
        content: String,
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
    sources: Vec<SourceReference>,
}

#[derive(Clone, Debug)]
struct ParsedConnection {
    connection: Connection,
    line_number: usize,
}

pub fn validate(output: &str) -> Result<(), GraphDiagnostic> {
    parse(output).map(|_| ())
}

pub fn validate_with_sources(
    output: &str,
    source_files: &[SourceFile],
) -> Result<(), GraphDiagnostic> {
    validate_with_sources_in_chunk(output, source_files, None)
}

pub fn validate_with_sources_in_chunk(
    output: &str,
    source_files: &[SourceFile],
    current_chunk: Option<&str>,
) -> Result<(), GraphDiagnostic> {
    let graph = parse(output)?;
    let source_line_counts = source_files
        .iter()
        .map(|file| (source_file_path(file), file.contents.lines().count()))
        .collect::<HashMap<_, _>>();
    let partial_ids = graph
        .partials
        .iter()
        .map(|partial| partial.id.as_str())
        .collect::<HashSet<_>>();

    for node in &graph.nodes {
        if partial_ids.contains(node.id.as_str()) {
            continue;
        }
        let (line_number, line) = node_declaration_line(output, &node.id);
        if node.sources.is_empty() {
            return Err(diagnostic(
                DiagnosticKind::SourceReference,
                line_number,
                line,
                &format!("Node `{}` does not have a source reference.", node.id),
                "Add a `<source>path:full</source>` or `<source>path:start-end</source>` tag that points to supporting code.",
            ));
        }

        for reference in &node.sources {
            let Some(line_count) = source_line_counts.get(&reference.path) else {
                return Err(diagnostic(
                    DiagnosticKind::SourceReference,
                    line_number,
                    line,
                    &format!(
                        "Node `{}` references `{}`, but that file is not present in the provided codebase.",
                        node.id, reference.path
                    ),
                    "Use a path to a file present in the provided codebase.",
                ));
            };
            if let Some(end) = reference.end
                && end > *line_count
            {
                return Err(diagnostic(
                    DiagnosticKind::SourceReference,
                    line_number,
                    line,
                    &format!(
                        "Node `{}` references lines {}-{} of `{}`, but the file has only {} lines.",
                        node.id,
                        reference.start.unwrap_or(1),
                        end,
                        reference.path,
                        line_count
                    ),
                    &format!(
                        "Choose an inclusive line range between 1 and {line_count}, or use `{}:full`.",
                        reference.path
                    ),
                ));
            }
        }
    }

    for partial in &graph.partials {
        let line = output
            .lines()
            .nth(partial.line_number.saturating_sub(1))
            .unwrap_or("");
        let Some(current_chunk) = current_chunk else {
            return Err(diagnostic(
                DiagnosticKind::PartialReference,
                partial.line_number,
                line,
                "The graph still contains an unresolved partial connection outside chunked generation.",
                "Replace every `<partial>...</partial>` element with a complete graph node or remove it.",
            ));
        };
        if partial.content.is_empty() || !current_chunk.contains(&partial.content) {
            return Err(diagnostic(
                DiagnosticKind::PartialReference,
                partial.line_number,
                line,
                &format!(
                    "Partial connection `{}` does not match code in the current chunk.",
                    partial.content
                ),
                "Use the exact invocation line from the current chunk inside `<partial>...</partial>`.",
            ));
        }
    }

    Ok(())
}

pub fn validate_no_partials(output: &str) -> Result<(), GraphDiagnostic> {
    let graph = parse(output)?;
    if let Some(partial) = graph.partials.first() {
        let line = output
            .lines()
            .nth(partial.line_number.saturating_sub(1))
            .unwrap_or("");
        return Err(diagnostic(
            DiagnosticKind::PartialReference,
            partial.line_number,
            line,
            &format!(
                "Partial connection `{}` remains after the last repository chunk.",
                partial.content
            ),
            "There is no more code to inspect; remove the partial element or transform it into a complete graph node and connection.",
        ));
    }
    Ok(())
}

pub fn parse(output: &str) -> Result<Graph, GraphDiagnostic> {
    let lines = output.lines().collect::<Vec<_>>();
    let mut header_seen = false;
    let mut wrapper_seen = false;
    let mut wrapper_closed = false;
    let mut graph_name = None;
    let mut graph_description = None;
    let mut declarations = HashSet::new();
    let mut nodes = Vec::new();
    let mut partials = Vec::new();
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
            let (_, name, description) =
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

        let tokens = tokenize_line(line, line_number).map_err(|explanation| {
            diagnostic(
                DiagnosticKind::Syntax,
                line_number,
                raw_line,
                &explanation,
                "Close every node label with its matching bracket and close any quoted label text.",
            )
        })?;

        for token in &tokens {
            match token {
                Token::Node { id, declaration } => {
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
                            sources: label.sources.clone(),
                        });
                    }
                }
                Token::Partial { id, content } => {
                    if !declarations.insert(id.clone()) {
                        return Err(diagnostic(
                            DiagnosticKind::Syntax,
                            line_number,
                            raw_line,
                            "A partial marker ID is duplicated.",
                            "Give every partial marker its own position in the graph.",
                        ));
                    }
                    nodes.push(GraphNode {
                        id: id.clone(),
                        name: Some("Unresolved partial connection".to_owned()),
                        description: Some(content.clone()),
                        sources: Vec::new(),
                    });
                    partials.push(GraphPartial {
                        id: id.clone(),
                        content: content.clone(),
                        line_number,
                    });
                }
                Token::Operator { .. } => {}
            }
        }

        let mut previous_node: Option<String> = None;
        let mut previous_operator: Option<(String, String)> = None;
        let mut line_edges = Vec::new();
        for token in &tokens {
            match token {
                Token::Node { id, .. } | Token::Partial { id, .. } => {
                    if let Some((syntax, label)) = previous_operator {
                        let from = previous_node
                            .as_deref()
                            .expect("operator validation requires a source node");
                        let connection = canonical_connection(from, id, &syntax, &label);
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
                        let (edge_from, edge_to) = if matches!(syntax.as_str(), "<--" | "<-.-") {
                            (id.as_str(), from)
                        } else {
                            (from, id.as_str())
                        };
                        line_edges.push(GraphEdge {
                            from: edge_from.to_owned(),
                            to: edge_to.to_owned(),
                            label: label.to_owned(),
                            dotted: syntax.contains('.'),
                            arrow_start: matches!(syntax.as_str(), "<-->" | "<-.->"),
                            arrow_end: !matches!(syntax.as_str(), "---" | "-.-"),
                        });
                        previous_operator = None;
                        previous_node = Some(id.clone());
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
                        previous_node = Some(id.clone());
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
                    previous_operator = Some((syntax.clone(), label.clone()));
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
        direction: GraphDirection::TopDown,
        name: graph_name,
        description: graph_description,
        nodes,
        edges: graph_edges,
        partials,
    })
}

fn source_file_path(file: &SourceFile) -> String {
    let mut filename = file.name.clone();
    if let Some(extension) = &file.extension {
        filename.push('.');
        filename.push_str(extension);
    }
    file.path
        .join(filename)
        .display()
        .to_string()
        .replace('\\', "/")
}

fn node_declaration_line<'a>(output: &'a str, node_id: &str) -> (usize, &'a str) {
    output
        .lines()
        .enumerate()
        .find_map(|(index, line)| {
            let tokens = tokenize_line(line.trim(), index + 1).ok()?;
            tokens.into_iter().find_map(|token| match token {
                Token::Node {
                    id,
                    declaration: Some(_),
                } if id == node_id => Some((index + 1, line)),
                _ => None,
            })
        })
        .unwrap_or((1, output.lines().next().unwrap_or("")))
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

fn tokenize_line(line: &str, line_number: usize) -> Result<Vec<Token>, String> {
    let bytes = line.as_bytes();
    let mut tokens = Vec::new();
    let mut index = 0;
    let mut partial_index = 0;

    while index < bytes.len() {
        if bytes[index..].starts_with(b"<partial>") {
            let content_start = index + "<partial>".len();
            let Some(relative_end) = line[content_start..].find("</partial>") else {
                return Err("A partial marker opens with `<partial>` but is not closed.".to_owned());
            };
            let content_end = content_start + relative_end;
            let content = line[content_start..content_end].trim().to_owned();
            tokens.push(Token::Partial {
                id: format!("__partial_{line_number}_{partial_index}"),
                content,
            });
            partial_index += 1;
            index = content_end + "</partial>".len();
            continue;
        }
        if bytes[index..].starts_with(b"</partial>") || bytes[index..].starts_with(b"<partial") {
            return Err(
                "A partial marker tag is malformed or has no matching opening tag.".to_owned(),
            );
        }

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
    let (name, name_position) = parse_label_tag(label, "name")?;
    let (description, description_position) = parse_label_tag(label, "desc")?;
    let (source, source_position) = parse_label_tag(label, "source")?;
    if name.is_none() && description.is_none() {
        return Err(
            "The node label must contain a complete `<name>...</name>` tag, a complete `<desc>...</desc>` tag, or both."
                .to_owned(),
        );
    }

    let positions = [name_position, description_position, source_position]
        .into_iter()
        .flatten()
        .collect::<Vec<_>>();
    if positions.windows(2).any(|pair| pair[0] > pair[1]) {
        return Err(
            "The node label tags must appear in name, description, source order.".to_owned(),
        );
    }

    let sources = match source {
        Some(source) => parse_source_references(&source)?,
        None => Vec::new(),
    };
    Ok(NodeLabel {
        name,
        description,
        sources,
    })
}

fn parse_label_tag(label: &str, tag: &str) -> Result<(Option<String>, Option<usize>), String> {
    let open = format!("<{tag}>");
    let close = format!("</{tag}>");
    let open_position = label.find(&open);
    let close_position = label.find(&close);
    match (open_position, close_position) {
        (None, None) => Ok((None, None)),
        (Some(start), Some(end)) if start < end => {
            let content_start = start + open.len();
            if label[content_start..].contains(&open) || label[..end].contains(&close) {
                return Err(format!(
                    "The node label contains more than one `<{tag}>` tag."
                ));
            }
            Ok((
                Some(label[content_start..end].trim().to_owned()),
                Some(start),
            ))
        }
        _ => Err(format!("The node label has an incomplete `<{tag}>` tag.")),
    }
}

fn parse_source_references(source: &str) -> Result<Vec<SourceReference>, String> {
    let mut references = Vec::new();
    for entry in source
        .split(',')
        .map(str::trim)
        .filter(|entry| !entry.is_empty())
    {
        let (path, range) = entry.rsplit_once(':').ok_or_else(|| {
            "Source references must use `path:full` or `path:start-end` syntax.".to_owned()
        })?;
        let path = path.trim().replace('\\', "/");
        if path.is_empty()
            || path.starts_with('/')
            || path
                .split('/')
                .any(|part| part.is_empty() || part == "." || part == "..")
        {
            return Err(
                "Source reference paths must be relative paths inside the codebase.".to_owned(),
            );
        }
        let (start, end) = if range == "full" {
            (None, None)
        } else {
            let (start, end) = range
                .split_once('-')
                .ok_or_else(|| "Source line ranges must use `path:start-end` syntax.".to_owned())?;
            let start = start
                .parse::<usize>()
                .ok()
                .filter(|line| *line > 0)
                .ok_or_else(|| {
                    "Source reference line numbers must be positive integers.".to_owned()
                })?;
            let end = end
                .parse::<usize>()
                .ok()
                .filter(|line| *line >= start)
                .ok_or_else(|| {
                    "Source range end must be an integer greater than or equal to its start."
                        .to_owned()
                })?;
            (Some(start), Some(end))
        };
        references.push(SourceReference { path, start, end });
    }
    if references.is_empty() {
        return Err("A source tag must contain at least one source reference.".to_owned());
    }
    Ok(references)
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
    use std::path::PathBuf;

    const NODE_DECLARATIONS: &str = r#"Alpha["<name>Alpha</name><desc>First node</desc>"]
Beta["<name>Beta</name><desc>Second node</desc>"]
"#;

    fn source_file(path: &str, name: &str, extension: &str, contents: &str) -> SourceFile {
        SourceFile {
            path: PathBuf::from(path),
            name: name.to_owned(),
            extension: Some(extension.to_owned()),
            contents: contents.to_owned(),
        }
    }

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
    fn parses_multiple_full_file_and_line_range_sources() {
        let graph = r#"<graph type="TD">
Node["<name>Node</name><desc>Details</desc><source>src/main.rs:full,src/lib.rs:10-24</source>"]
</graph>"#;

        let parsed = parse(graph).unwrap();
        assert_eq!(
            parsed.nodes[0].sources,
            vec![
                SourceReference {
                    path: "src/main.rs".to_owned(),
                    start: None,
                    end: None,
                },
                SourceReference {
                    path: "src/lib.rs".to_owned(),
                    start: Some(10),
                    end: Some(24),
                },
            ]
        );
    }

    #[test]
    fn rejects_unsafe_or_invalid_source_references() {
        for source in ["../secret.rs:full", "src/main.rs:0-5", "src/main.rs:9-4"] {
            let graph = format!(
                "<graph type=\"TD\">\nNode[\"<name>Node</name><source>{source}</source>\"]\n</graph>"
            );
            assert!(
                validate(&graph).is_err(),
                "accepted invalid source {source}"
            );
        }
    }

    #[test]
    fn validates_source_paths_and_inclusive_line_ranges() {
        let files = [source_file("src", "main", "rs", "one\ntwo\nthree\n")];
        let graph = r#"<graph type="TD">
Node["<name>Node</name><source>src/main.rs:2-3</source>"]
</graph>"#;

        assert_eq!(validate_with_sources(graph, &files), Ok(()));
    }

    #[test]
    fn rejects_source_paths_not_present_in_the_provided_files() {
        let graph = r#"<graph type="TD">
Node["<name>Node</name><source>src/missing.rs:full</source>"]
</graph>"#;

        let error = validate_with_sources(graph, &[]).unwrap_err();
        assert_eq!(error.kind, DiagnosticKind::SourceReference);
        assert_eq!(error.line_number, 2);
        assert!(error.explanation.contains("src/missing.rs"));
        assert!(error.expected.contains("provided codebase"));
    }

    #[test]
    fn rejects_source_ranges_that_exceed_the_file_line_count() {
        let files = [source_file("src", "main", "rs", "one\ntwo\nthree\n")];
        let graph = r#"<graph type="TD">
Node["<name>Node</name><source>src/main.rs:2-4</source>"]
</graph>"#;

        let error = validate_with_sources(graph, &files).unwrap_err();
        assert_eq!(error.kind, DiagnosticKind::SourceReference);
        assert_eq!(error.line_number, 2);
        assert!(error.explanation.contains("only 3 lines"));
        assert!(error.expected.contains("between 1 and 3"));
    }

    #[test]
    fn requires_each_node_to_have_a_source_reference() {
        let files = [source_file("src", "main", "rs", "fn main() {}\n")];
        let graph = r#"<graph type="TD">
Node["<name>Node</name>"]
</graph>"#;

        let error = validate_with_sources(graph, &files).unwrap_err();
        assert_eq!(error.kind, DiagnosticKind::SourceReference);
        assert_eq!(error.line_number, 2);
        assert!(
            error
                .explanation
                .contains("does not have a source reference")
        );
    }

    #[test]
    fn parses_partial_connections_as_temporary_graph_endpoints() {
        let graph = r#"<graph type="TD">
Alpha["<name>Alpha</name><source>main.rs:full</source>"] --> <partial>1 call()</partial>
</graph>"#;

        let parsed = parse(graph).unwrap();
        assert_eq!(parsed.partials.len(), 1);
        assert_eq!(parsed.partials[0].content, "1 call()");
        assert_eq!(parsed.edges.len(), 1);
        assert_eq!(parsed.edges[0].to, parsed.partials[0].id);
    }

    #[test]
    fn validates_partial_lines_against_the_current_chunk() {
        let files = [source_file("", "main", "rs", "call()\n")];
        let graph = r#"<graph type="TD">
Alpha["<name>Alpha</name><source>main.rs:full</source>"] --> <partial>1 call()</partial>
</graph>"#;
        let current_chunk = "``` main.rs\n1 call()\n```\n\n";

        assert_eq!(
            validate_with_sources_in_chunk(graph, &files, Some(current_chunk)),
            Ok(())
        );
        let error =
            validate_with_sources_in_chunk(graph, &files, Some("different code")).unwrap_err();
        assert_eq!(error.kind, DiagnosticKind::PartialReference);
    }

    #[test]
    fn rejects_partial_elements_in_complete_graph_validation() {
        let files = [source_file("", "main", "rs", "call()\n")];
        let graph = r#"<graph type="TD">
Alpha["<name>Alpha</name><source>main.rs:full</source>"] --> <partial>1 call()</partial>
</graph>"#;

        assert_eq!(
            validate_with_sources(graph, &files).unwrap_err().kind,
            DiagnosticKind::PartialReference
        );
        assert_eq!(
            validate_no_partials(graph).unwrap_err().kind,
            DiagnosticKind::PartialReference
        );
    }

    #[test]
    fn accepts_graph_wrapper_metadata_and_inline_node_declarations() {
        let graph = r#"<graph type="LR" name="Custom data processor layer" desc="Checks data against groundtruth">
Input["<name>Input</name><desc>Parse input</desc>"] --> Processor["<name>Processor</name><desc>Run calculations</desc>"]
Processor --> Output["<name>Output</name>"]
</graph>"#;

        let parsed = parse(graph).unwrap();
        assert_eq!(parsed.direction, GraphDirection::TopDown);
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
