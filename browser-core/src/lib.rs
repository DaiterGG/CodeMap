use futures_util::StreamExt;
use ignore::Match;
use ignore::gitignore::{Gitignore, GitignoreBuilder};
use js_sys::Function;
use serde::{Deserialize, Serialize};
use std::path::{Path, PathBuf};
use wasm_bindgen::prelude::*;

pub mod files {
    use std::path::PathBuf;

    pub struct SourceFile {
        pub path: PathBuf,
        pub name: String,
        pub extension: Option<String>,
        pub contents: String,
    }
}

#[path = "../../src/graph_parser.rs"]
pub mod graph_parser;

const CHUNK_CONTEXT_PERCENT: usize = 80;
const MAX_RETRIES: usize = 3;
const MAX_PARALLEL_CHUNKS: usize = 3;
const NODEGRAPH_PROMPT: &str = include_str!("../../prompts/nodegraph.txt");
const START_PROMPT: &str = include_str!("../../prompts/multiagent_start.txt");
const SEQUENTIAL_PROMPT: &str = include_str!("../../prompts/multiagent_sequential.txt");
const PARALLEL_PROMPT: &str = include_str!("../../prompts/multiagent_parallel.txt");
const SEGMENT_PROMPT: &str = "A file may be provided as a segment. Its displayed line numbers are the original file line numbers; preserve those numbers in source references and do not assume the segment begins at line 1.";

#[derive(Deserialize)]
struct Request {
    source: String,
    files: Vec<FileInput>,
}

#[derive(Clone, Deserialize, Serialize)]
struct FileInput {
    path: String,
    contents: String,
}

#[derive(Deserialize)]
struct IgnoreRequest {
    files: Vec<PathInput>,
    gitignores: Vec<FileInput>,
}

#[derive(Deserialize)]
struct PathInput {
    path: String,
}

struct IgnoreMatcher {
    base: PathBuf,
    matcher: Gitignore,
}

#[derive(Deserialize)]
struct GenerationRequest {
    files: Vec<FileInput>,
    model: String,
    api_key: String,
    #[serde(default)]
    api_url: String,
    context_length: usize,
    parallel: bool,
    referer: String,
}

#[derive(Deserialize)]
struct SummaryRequest {
    nodes: Vec<SummaryNode>,
    edges: Vec<SummaryEdge>,
    node_id: String,
    files: Vec<FileInput>,
    model: String,
    api_key: String,
    #[serde(default)]
    api_url: String,
    context_length: usize,
    referer: String,
}

#[derive(Deserialize)]
struct SummaryNode {
    id: String,
    name: Option<String>,
    #[serde(default)]
    sources: Vec<graph_parser::SourceReference>,
}

#[derive(Deserialize)]
struct SummaryEdge {
    from: String,
    to: String,
}

fn openrouter_endpoint(api_url: &str) -> &str {
    if api_url.trim().is_empty() {
        "https://openrouter.ai/api/v1/chat/completions"
    } else {
        api_url
    }
}

#[derive(Debug)]
struct SummaryPrompt {
    prompt: String,
    estimated_bytes: usize,
    context_limit_bytes: usize,
    included_connected_nodes: usize,
    total_connected_nodes: usize,
}

#[derive(Serialize)]
struct SummaryResult {
    summary: String,
    estimated_bytes: usize,
    context_limit_bytes: usize,
    included_connected_nodes: usize,
    total_connected_nodes: usize,
}

#[derive(Clone, Serialize)]
struct ChatMessage {
    role: &'static str,
    content: String,
}

impl ChatMessage {
    fn system(content: String) -> Self {
        Self {
            role: "system",
            content,
        }
    }

    fn user(content: String) -> Self {
        Self {
            role: "user",
            content,
        }
    }

    fn assistant(content: String) -> Self {
        Self {
            role: "assistant",
            content,
        }
    }
}

#[derive(Serialize)]
struct HistoryEntry {
    messages: Vec<ChatMessage>,
    response: String,
    validation_error: Option<String>,
}

#[derive(Serialize)]
struct GenerationResult {
    graph: graph_parser::Graph,
    source: String,
    history: Vec<HistoryEntry>,
}

struct ChunkGeneration {
    response: CompletionMessage,
    history: Vec<HistoryEntry>,
}

#[derive(Clone)]
struct CompletionMessage {
    content: String,
    reasoning: Option<String>,
}

#[wasm_bindgen]
pub fn validate_graph(request: &str) -> Result<String, JsValue> {
    let request = serde_json::from_str::<Request>(request)
        .map_err(|error| JsValue::from_str(&format!("Invalid request: {error}")))?;
    serde_json::to_string(&validate(request))
        .map_err(|error| JsValue::from_str(&format!("Could not serialize graph: {error}")))
}

#[wasm_bindgen]
pub fn parse_graph_source(source: &str) -> Result<String, JsValue> {
    let graph = graph_parser::parse(source)
        .map_err(|error| JsValue::from_str(&error.correction_details()))?;
    serde_json::to_string(&graph)
        .map_err(|error| JsValue::from_str(&format!("Could not serialize graph: {error}")))
}

#[wasm_bindgen]
pub fn filter_ignored_files(request: &str) -> Result<String, JsValue> {
    let request = serde_json::from_str::<IgnoreRequest>(request)
        .map_err(|error| JsValue::from_str(&format!("Invalid request: {error}")))?;
    serde_json::to_string(&filter_ignored(request))
        .map_err(|error| JsValue::from_str(&format!("Could not serialize ignore results: {error}")))
}

#[wasm_bindgen]
pub async fn generate_multiagent(request: &str, progress: Function) -> Result<String, JsValue> {
    let request = serde_json::from_str::<GenerationRequest>(request)
        .map_err(|error| JsValue::from_str(&format!("Invalid generation request: {error}")))?;
    let result = generate(request, &progress)
        .await
        .map_err(|error| JsValue::from_str(&error))?;
    serde_json::to_string(&result).map_err(|error| {
        JsValue::from_str(&format!("Could not serialize generated graph: {error}"))
    })
}

#[wasm_bindgen]
pub async fn generate_node_summary(request: &str, progress: Function) -> Result<String, JsValue> {
    let request = serde_json::from_str::<SummaryRequest>(request)
        .map_err(|error| JsValue::from_str(&format!("Invalid summary request: {error}")))?;
    let result = generate_summary(request, &progress)
        .await
        .map_err(|error| JsValue::from_str(&error))?;
    serde_json::to_string(&result)
        .map_err(|error| JsValue::from_str(&format!("Could not serialize node summary: {error}")))
}

fn filter_ignored(request: IgnoreRequest) -> serde_json::Value {
    match build_ignore_matchers(request.gitignores).and_then(|matchers| {
        request
            .files
            .into_iter()
            .filter_map(|file| match is_ignored(&file.path, &matchers) {
                Ok(true) => Some(Ok(file.path)),
                Ok(false) => None,
                Err(error) => Some(Err(error)),
            })
            .collect::<Result<Vec<_>, _>>()
    }) {
        Ok(ignored) => serde_json::json!({ "ignored": ignored }),
        Err(error) => serde_json::json!({ "error": error }),
    }
}

fn build_ignore_matchers(gitignores: Vec<FileInput>) -> Result<Vec<IgnoreMatcher>, String> {
    let mut matchers = Vec::new();
    for file in gitignores {
        let path = PathBuf::from(&file.path);
        ensure_relative_path(&path)?;
        if path.file_name().is_none_or(|name| name != ".gitignore") {
            continue;
        }
        let parent = path.parent().unwrap_or_else(|| Path::new(""));
        let mut builder = GitignoreBuilder::new(if parent.as_os_str().is_empty() {
            Path::new(".")
        } else {
            parent
        });
        for line in file.contents.lines() {
            builder
                .add_line(Some(path.clone()), line)
                .map_err(|error| format!("invalid rule in {}: {error}", file.path))?;
        }
        let matcher = builder
            .build()
            .map_err(|error| format!("could not parse {}: {error}", file.path))?;
        matchers.push(IgnoreMatcher {
            base: parent.to_path_buf(),
            matcher,
        });
    }
    matchers.sort_by_key(|ignore| ignore.base.components().count());
    Ok(matchers)
}

fn is_ignored(path: &str, matchers: &[IgnoreMatcher]) -> Result<bool, String> {
    let path_buf = PathBuf::from(path);
    ensure_relative_path(&path_buf)?;
    let mut directories = path_buf
        .parent()
        .into_iter()
        .flat_map(Path::ancestors)
        .filter(|directory| !directory.as_os_str().is_empty())
        .collect::<Vec<_>>();
    directories.reverse();

    for directory in directories {
        let mut ignored = false;
        for matcher in matchers
            .iter()
            .filter(|matcher| is_base_ancestor(&matcher.base, directory, false))
        {
            let relative = relative_to_base(&matcher.base, directory)?;
            match matcher.matcher.matched(relative, true) {
                Match::Ignore(_) => ignored = true,
                Match::Whitelist(_) => ignored = false,
                Match::None => {}
            }
        }
        if ignored {
            return Ok(true);
        }
    }

    let mut ignored = false;
    for matcher in matchers
        .iter()
        .filter(|matcher| is_base_ancestor(&matcher.base, &path_buf, true))
    {
        let relative = relative_to_base(&matcher.base, &path_buf)?;
        match matcher.matcher.matched(relative, false) {
            Match::Ignore(_) => ignored = true,
            Match::Whitelist(_) => ignored = false,
            Match::None => {}
        }
    }
    Ok(ignored)
}

fn is_base_ancestor(base: &Path, path: &Path, allow_equal: bool) -> bool {
    if base.as_os_str().is_empty() {
        return true;
    }
    path.starts_with(base) && (allow_equal || path != base)
}

fn relative_to_base<'a>(base: &Path, path: &'a Path) -> Result<&'a Path, String> {
    if base.as_os_str().is_empty() {
        Ok(path)
    } else {
        path.strip_prefix(base)
            .map_err(|error| format!("could not match path {}: {error}", path.display()))
    }
}

fn ensure_relative_path(path: &Path) -> Result<(), String> {
    if path.is_absolute()
        || path
            .components()
            .any(|component| !matches!(component, std::path::Component::Normal(_)))
    {
        return Err(format!("invalid project-relative path: {}", path.display()));
    }
    Ok(())
}

fn validate(request: Request) -> serde_json::Value {
    let files = request
        .files
        .into_iter()
        .map(|file| source_file(&file.path, file.contents))
        .collect::<Vec<_>>();
    if let Err(diagnostic) = graph_parser::validate_with_sources(&request.source, &files) {
        return serde_json::json!({ "error": diagnostic.correction_details() });
    }
    match graph_parser::parse(&request.source) {
        Ok(graph) => serde_json::json!({ "graph": graph }),
        Err(diagnostic) => serde_json::json!({ "error": diagnostic.correction_details() }),
    }
}

async fn generate_summary(
    request: SummaryRequest,
    progress: &Function,
) -> Result<SummaryResult, String> {
    if request.api_key.trim().is_empty() && request.api_url.trim().is_empty() {
        return Err("Sign in to OpenRouter before generating a summary.".to_owned());
    }
    if request.model.trim().is_empty() {
        return Err("Choose a model before generating a summary.".to_owned());
    }
    if request.context_length == 0 {
        return Err("The selected model does not provide a usable context length.".to_owned());
    }
    let prepared = build_summary_prompt(&request)?;
    let estimate = serde_json::json!({
        "estimated_bytes": prepared.estimated_bytes,
        "context_limit_bytes": prepared.context_limit_bytes,
        "included_connected_nodes": prepared.included_connected_nodes,
        "total_connected_nodes": prepared.total_connected_nodes
    });
    progress
        .call1(&JsValue::NULL, &JsValue::from_str(&estimate.to_string()))
        .map_err(|error| {
            error
                .as_string()
                .unwrap_or_else(|| "Browser progress callback failed.".to_owned())
        })?;

    let mut builder = reqwest::Client::new()
        .post(openrouter_endpoint(&request.api_url))
        .header("Content-Type", "application/json")
        .header("HTTP-Referer", &request.referer)
        .header("X-OpenRouter-Title", "CodeMap");
    if !request.api_key.trim().is_empty() {
        builder = builder.bearer_auth(&request.api_key);
    }
    let response = builder
        .json(&serde_json::json!({
            "model": request.model,
            "messages": [{"role": "user", "content": prepared.prompt}],
            "max_tokens": 1024
        }))
        .send()
        .await
        .map_err(|error| format!("OpenRouter summary request failed: {error}"))?;
    let status = response.status();
    let body = response
        .text()
        .await
        .map_err(|error| format!("Could not read OpenRouter summary response: {error}"))?;
    if !status.is_success() {
        return Err(format!("OpenRouter returned {status}: {body}"));
    }
    let completion: ChatCompletionResponse = serde_json::from_str(&body)
        .map_err(|error| format!("Could not decode OpenRouter summary response: {error}"))?;
    let summary = completion
        .choices
        .into_iter()
        .next()
        .and_then(|choice| choice.message.content)
        .filter(|content| !content.trim().is_empty())
        .ok_or_else(|| "OpenRouter response contained no summary content.".to_owned())?;

    Ok(SummaryResult {
        summary,
        estimated_bytes: prepared.estimated_bytes,
        context_limit_bytes: prepared.context_limit_bytes,
        included_connected_nodes: prepared.included_connected_nodes,
        total_connected_nodes: prepared.total_connected_nodes,
    })
}

fn build_summary_prompt(request: &SummaryRequest) -> Result<SummaryPrompt, String> {
    let current_node = request
        .nodes
        .iter()
        .find(|node| node.id == request.node_id)
        .ok_or_else(|| format!("Node '{}' is not present in the graph.", request.node_id))?;
    let connected_nodes = request
        .nodes
        .iter()
        .filter(|candidate| {
            candidate.id != request.node_id
                && request.edges.iter().any(|edge| {
                    edge.from == request.node_id && edge.to == candidate.id
                        || edge.to == request.node_id && edge.from == candidate.id
                })
        })
        .collect::<Vec<_>>();
    let sources = request
        .files
        .iter()
        .map(|file| (file.path.as_str(), file.contents.as_str()))
        .collect::<std::collections::HashMap<_, _>>();
    let prompt_instruction = format!(
        "Generate a concise summary of ONLY the node {} ({}). Use the other listed nodes only as context for its direct relationships; do not summarize them.",
        current_node.name.as_deref().unwrap_or(&current_node.id),
        current_node.id
    );
    let context_limit_bytes = request.context_length.saturating_mul(90) / 100;
    let mut contexts = Vec::new();
    let mut included_connected_nodes = 0;

    for (index, node) in std::iter::once(current_node)
        .chain(connected_nodes.iter().copied())
        .enumerate()
    {
        let mut chunks = Vec::new();
        for reference in &node.sources {
            let source = sources.get(reference.path.as_str()).ok_or_else(|| {
                format!(
                    "This graph's source file '{}' is not loaded in the browser.",
                    reference.path
                )
            })?;
            let lines = source.split('\n').collect::<Vec<_>>();
            let start = reference.start.unwrap_or(1);
            let end = reference.end.unwrap_or(lines.len());
            if start == 0 || end < start || end > lines.len() {
                return Err(format!(
                    "Source line range for {} is outside the file.",
                    reference.path
                ));
            }
            let body = lines[start - 1..end]
                .iter()
                .map(|line| line.trim_end_matches('\r'))
                .collect::<Vec<_>>()
                .join("\n");
            chunks.push(format!("```{}\n{}\n```", reference.path, body));
        }
        let node_context = format!("<{}>\n{}</{}>", node.id, chunks.join("\n\n"), node.id);
        let mut candidate = contexts.clone();
        candidate.push(node_context.clone());
        candidate.push(prompt_instruction.clone());
        let candidate_prompt = candidate.join("\n\n");
        let candidate_bytes = candidate_prompt.len();
        if candidate_bytes > context_limit_bytes {
            if index == 0 {
                return Err(format!(
                    "The current node's source context is estimated at {candidate_bytes} bytes, exceeding 90% of the selected model's context window ({context_limit_bytes} bytes). Select a model with a larger context window."
                ));
            }
            break;
        }
        contexts.push(node_context);
        if index > 0 {
            included_connected_nodes += 1;
        }
    }

    let prompt = contexts
        .into_iter()
        .chain(std::iter::once(prompt_instruction))
        .collect::<Vec<_>>()
        .join("\n\n");
    Ok(SummaryPrompt {
        estimated_bytes: prompt.len(),
        context_limit_bytes,
        included_connected_nodes,
        total_connected_nodes: connected_nodes.len(),
        prompt,
    })
}

async fn generate(
    request: GenerationRequest,
    progress: &Function,
) -> Result<GenerationResult, String> {
    if request.api_key.trim().is_empty() && request.api_url.trim().is_empty() {
        return Err("Sign in to OpenRouter before generating a graph.".to_owned());
    }
    if request.model.trim().is_empty() {
        return Err("Choose a model before generating a graph.".to_owned());
    }
    if request.context_length == 0 {
        return Err("The selected model does not provide a usable context length.".to_owned());
    }

    let source_files = request
        .files
        .iter()
        .map(|file| source_file(&file.path, file.contents.clone()))
        .collect::<Vec<_>>();
    if source_files.is_empty() {
        return Err("Select a folder containing readable source files.".to_owned());
    }

    let full_source = format_source_files(&request.files);
    if !request.parallel
        && full_source.len().saturating_add(NODEGRAPH_PROMPT.len()) <= request.context_length
    {
        return generate_single(&request, &source_files, full_source, progress).await;
    }

    let source_budget = request.context_length.saturating_mul(CHUNK_CONTEXT_PERCENT) / 100;
    let chunks = collect_chunks(&request.files, source_budget)?;
    if chunks.is_empty() {
        return Err(
            "No source files fit in the model context. Select fewer files or a model with a larger context."
                .to_owned(),
        );
    }

    let client = reqwest::Client::new();
    let mut generations = if request.parallel {
        report(
            progress,
            format!("Generating {} chunks in parallel...", chunks.len()),
        )?;
        futures_util::stream::iter(chunks.iter().enumerate())
            .map(|(index, chunk)| {
                generate_chunk(
                    &client,
                    &request,
                    &source_files,
                    chunk,
                    None,
                    index,
                    chunks.len(),
                    progress,
                )
            })
            .buffered(MAX_PARALLEL_CHUNKS)
            .collect::<Vec<_>>()
            .await
            .into_iter()
            .collect::<Result<Vec<_>, _>>()?
    } else {
        let mut results = Vec::with_capacity(chunks.len());
        let mut previous_agent = None;
        for (index, chunk) in chunks.iter().enumerate() {
            let generated = generate_chunk(
                &client,
                &request,
                &source_files,
                chunk,
                previous_agent.as_ref(),
                index,
                chunks.len(),
                progress,
            )
            .await?;
            previous_agent = Some(generated.response.clone());
            results.push(generated);
        }
        results
    };

    let mut history = generations
        .iter_mut()
        .flat_map(|generation| std::mem::take(&mut generation.history))
        .collect::<Vec<_>>();
    let final_chunk = chunks.last().expect("non-empty chunks were checked above");
    let mut aggregation_messages = None;
    let mut response = if request.parallel {
        let messages = parallel_aggregation_messages(&generations);
        report(progress, "Combining parallel agent graphs...".to_owned())?;
        let response = complete_with_retries(&client, &request, &messages).await?;
        history.push(history_entry(messages.clone(), &response, None));
        aggregation_messages = Some(messages);
        response
    } else {
        generations
            .last()
            .expect("non-empty chunks were checked above")
            .response
            .clone()
    };

    for correction_attempt in 0..=MAX_RETRIES {
        if graph_validation(&response, &source_files).is_ok() {
            let graph = graph_parser::parse(&response.content)
                .map_err(|diagnostic| diagnostic.correction_details())?;
            report(progress, "Graph validated.".to_owned())?;
            return Ok(GenerationResult {
                graph,
                source: response.content,
                history,
            });
        }

        let diagnostic = graph_validation(&response, &source_files)
            .expect_err("invalid graph validation must have a diagnostic");
        if correction_attempt == MAX_RETRIES {
            return Err(format!(
                "Could not finalize the graph after {MAX_RETRIES} corrections: {}",
                diagnostic.correction_details()
            ));
        }

        let correction = if diagnostic.kind == graph_parser::DiagnosticKind::PartialReference {
            format!(
                "There is no more code to inspect. Remove every remaining `<partial>...</partial>` element or transform it into a complete graph node and connection. Preserve the valid graph. Return only the complete corrected graph.\n\n{}",
                diagnostic.correction_details()
            )
        } else {
            correction_for(&diagnostic)
        };
        let mut messages = if let Some(aggregation_messages) = &aggregation_messages {
            let mut messages = aggregation_messages.clone();
            messages.push(previous_agent(&response));
            messages
        } else {
            base_messages(final_chunk, None)
        };
        messages.push(ChatMessage::user(correction));
        report(
            progress,
            format!(
                "Correcting final graph ({}/{MAX_RETRIES})...",
                correction_attempt + 1
            ),
        )?;
        let corrected = complete_with_retries(&client, &request, &messages).await?;
        let validation_error = graph_validation(&corrected, &source_files)
            .err()
            .map(|diagnostic| diagnostic.correction_details());
        history.push(history_entry(messages, &corrected, validation_error));
        response = corrected;
    }

    Err("Graph generation ended without a validated graph.".to_owned())
}

async fn generate_single(
    request: &GenerationRequest,
    source_files: &[files::SourceFile],
    source_context: String,
    progress: &Function,
) -> Result<GenerationResult, String> {
    let client = reqwest::Client::new();
    let mut messages = single_messages(source_context);
    let mut history = Vec::new();

    for attempt in 0..=MAX_RETRIES {
        report(
            progress,
            format!(
                "Generating graph (attempt {}/{})...",
                attempt + 1,
                MAX_RETRIES + 1
            ),
        )?;
        let response = complete_with_retries(&client, request, &messages).await?;
        let validation_error = graph_parser::validate_with_sources(&response.content, source_files)
            .err()
            .map(|diagnostic| diagnostic.correction_details());
        history.push(history_entry(
            messages.clone(),
            &response,
            validation_error.clone(),
        ));
        if validation_error.is_none() {
            let graph = graph_parser::parse(&response.content)
                .map_err(|diagnostic| diagnostic.correction_details())?;
            report(progress, "Graph validated.".to_owned())?;
            return Ok(GenerationResult {
                graph,
                source: response.content,
                history,
            });
        }
        if attempt == MAX_RETRIES {
            return Err(format!(
                "Could not generate a valid graph after {MAX_RETRIES} corrections: {}",
                validation_error.expect("failed validation includes a diagnostic")
            ));
        }
        let diagnostic = graph_parser::validate_with_sources(&response.content, source_files)
            .expect_err("the invalid graph has a diagnostic");
        messages.push(ChatMessage::assistant(response.content));
        messages.push(ChatMessage::user(correction_for(&diagnostic)));
    }

    Err("Single-agent generation ended without a validated graph.".to_owned())
}

fn single_messages(source_context: String) -> Vec<ChatMessage> {
    vec![
        ChatMessage::system(source_context),
        ChatMessage::user(NODEGRAPH_PROMPT.to_owned()),
    ]
}

async fn generate_chunk(
    client: &reqwest::Client,
    request: &GenerationRequest,
    source_files: &[files::SourceFile],
    chunk: &str,
    previous: Option<&CompletionMessage>,
    index: usize,
    total: usize,
    progress: &Function,
) -> Result<ChunkGeneration, String> {
    let mut history = Vec::new();
    let mut previous_failure: Option<(CompletionMessage, String)> = None;
    for attempt in 0..=MAX_RETRIES {
        let mut messages = base_messages(chunk, previous);
        if let Some((response, correction)) = &previous_failure {
            messages.push(previous_agent(response));
            messages.push(ChatMessage::user(correction.clone()));
        }
        report(
            progress,
            format!(
                "Processing chunk {}/{} (attempt {}/{})...",
                index + 1,
                total,
                attempt + 1,
                MAX_RETRIES + 1
            ),
        )?;
        let response = complete_with_retries(client, request, &messages).await?;
        let validation = graph_parser::validate_with_sources_in_chunk(
            &response.content,
            source_files,
            Some(chunk),
        );
        let validation_error = validation
            .as_ref()
            .err()
            .map(|diagnostic| diagnostic.correction_details());
        history.push(history_entry(messages, &response, validation_error.clone()));
        if validation_error.is_none() {
            return Ok(ChunkGeneration { response, history });
        }
        if attempt == MAX_RETRIES {
            return Err(format!(
                "Chunk {}/{} failed validation after {MAX_RETRIES} corrections: {}",
                index + 1,
                total,
                validation_error.expect("failed validation includes a diagnostic")
            ));
        }
        let diagnostic = validation.expect_err("failed validation includes a diagnostic");
        previous_failure = Some((response, correction_for(&diagnostic)));
    }
    Err("Chunk generation ended without a validated partial graph.".to_owned())
}

async fn complete_with_retries(
    client: &reqwest::Client,
    request: &GenerationRequest,
    messages: &[ChatMessage],
) -> Result<CompletionMessage, String> {
    let mut last_error = String::new();
    for _attempt in 0..=MAX_RETRIES {
        let result = async {
            let mut builder = client
                .post(openrouter_endpoint(&request.api_url))
                .header("Content-Type", "application/json")
                .header("HTTP-Referer", &request.referer)
                .header("X-OpenRouter-Title", "CodeMap");
            if !request.api_key.trim().is_empty() {
                builder = builder.bearer_auth(&request.api_key);
            }
            let builder = builder.json(&serde_json::json!({
                "model": request.model,
                "messages": messages
            }));
            let response = builder
                .send()
                .await
                .map_err(|error| format!("OpenRouter request failed: {error}"))?;
            let status = response.status();
            let body = response
                .text()
                .await
                .map_err(|error| format!("Could not read OpenRouter response: {error}"))?;
            if !status.is_success() {
                return Err(format!("OpenRouter returned {status}: {body}"));
            }
            let completion: ChatCompletionResponse = serde_json::from_str(&body)
                .map_err(|error| format!("Could not decode OpenRouter response: {error}"))?;
            let choice = completion
                .choices
                .into_iter()
                .next()
                .ok_or_else(|| "OpenRouter response contained no choices.".to_owned())?;
            let content = choice
                .message
                .content
                .filter(|content| !content.trim().is_empty())
                .ok_or_else(|| "OpenRouter response contained no message content.".to_owned())?;
            Ok(CompletionMessage {
                content,
                reasoning: choice.message.reasoning,
            })
        }
        .await;

        match result {
            Ok(response) => return Ok(response),
            Err(error) => {
                last_error = error;
            }
        }
    }
    Err(format!(
        "OpenRouter request failed after {MAX_RETRIES} retries: {last_error}"
    ))
}

#[derive(Deserialize)]
struct ChatCompletionResponse {
    choices: Vec<ChatChoice>,
}

#[derive(Deserialize)]
struct ChatChoice {
    message: ChatCompletionMessage,
}

#[derive(Deserialize)]
struct ChatCompletionMessage {
    content: Option<String>,
    reasoning: Option<String>,
}

fn base_messages(chunk: &str, previous: Option<&CompletionMessage>) -> Vec<ChatMessage> {
    let mut messages = vec![
        ChatMessage::system(chunk.to_owned()),
        ChatMessage::user(NODEGRAPH_PROMPT.to_owned()),
        ChatMessage::user(SEGMENT_PROMPT.to_owned()),
        ChatMessage::user(START_PROMPT.to_owned()),
    ];
    if let Some(previous) = previous {
        messages.push(previous_agent(previous));
        messages.push(ChatMessage::user(SEQUENTIAL_PROMPT.to_owned()));
    }
    messages
}

fn parallel_aggregation_messages(generations: &[ChunkGeneration]) -> Vec<ChatMessage> {
    let mut messages = vec![
        ChatMessage::system(NODEGRAPH_PROMPT.to_owned()),
        ChatMessage::user(SEGMENT_PROMPT.to_owned()),
        ChatMessage::user(START_PROMPT.to_owned()),
        ChatMessage::user(PARALLEL_PROMPT.to_owned()),
    ];
    let mut dump = String::new();
    for (index, generation) in generations.iter().enumerate() {
        dump.push_str(&format!(
            "Agent {} thought process:\n{}\n\nAgent {} graph output:\n{}\n\n",
            index + 1,
            generation
                .response
                .reasoning
                .as_deref()
                .unwrap_or("(no reasoning supplied)"),
            index + 1,
            generation.response.content
        ));
    }
    messages.push(ChatMessage::user(dump));
    messages
}

fn previous_agent(response: &CompletionMessage) -> ChatMessage {
    ChatMessage::assistant(format!(
        "Previous agent thought process:\n{}\n\nPrevious agent graph output:\n{}",
        response
            .reasoning
            .as_deref()
            .unwrap_or("(no reasoning supplied)"),
        response.content
    ))
}

fn history_entry(
    messages: Vec<ChatMessage>,
    response: &CompletionMessage,
    validation_error: Option<String>,
) -> HistoryEntry {
    HistoryEntry {
        messages,
        response: response.content.clone(),
        validation_error,
    }
}

fn graph_validation(
    response: &CompletionMessage,
    source_files: &[files::SourceFile],
) -> Result<(), graph_parser::GraphDiagnostic> {
    graph_parser::validate_with_sources(&response.content, source_files)
        .and_then(|_| graph_parser::validate_no_partials(&response.content))
}

fn correction_for(diagnostic: &graph_parser::GraphDiagnostic) -> String {
    let prompt = match diagnostic.kind {
        graph_parser::DiagnosticKind::Syntax => include_str!("../../prompts/syntax_error.txt"),
        graph_parser::DiagnosticKind::RedundantConnection => {
            include_str!("../../prompts/redundant_connection.txt")
        }
        graph_parser::DiagnosticKind::SourceReference => {
            include_str!("../../prompts/source_reference_error.txt")
        }
        graph_parser::DiagnosticKind::PartialReference => {
            "Correct the partial connection using the exact invocation line in the current chunk. Preserve valid graph content and return only the complete corrected graph."
        }
    };
    format!("{prompt}\n\n{}", diagnostic.correction_details())
}

fn format_source_files(source_files: &[FileInput]) -> String {
    let mut output = String::new();
    for file in source_files {
        output.push_str(&format!("``` {}\n", file.path));
        for (index, line) in file.contents.lines().enumerate() {
            output.push_str(&format!("{} {}\n", index + 1, line));
        }
        output.push_str("```\n\n");
    }
    output
}

fn collect_chunks(source_files: &[FileInput], source_budget: usize) -> Result<Vec<String>, String> {
    let mut units = Vec::new();
    for file in source_files {
        let formatted = format_source_files(std::slice::from_ref(file));
        if formatted.len() <= source_budget {
            units.push(formatted);
        } else {
            units.extend(split_file(file, source_budget)?);
        }
    }

    let mut chunks = Vec::new();
    let mut chunk = String::new();
    for unit in units {
        if !chunk.is_empty() && chunk.len() + unit.len() > source_budget {
            chunks.push(std::mem::take(&mut chunk));
        }
        chunk.push_str(&unit);
    }
    if !chunk.is_empty() {
        chunks.push(chunk);
    }
    Ok(chunks)
}

fn split_file(file: &FileInput, source_budget: usize) -> Result<Vec<String>, String> {
    let header = format!("``` {}\n", file.path.replace('\\', "/"));
    let footer = "```\n\n";
    let wrapper_bytes = header.len() + footer.len();
    if wrapper_bytes > source_budget {
        return Err(format!(
            "Source file '{}' cannot fit in the {source_budget}-byte chunk budget.",
            file.path
        ));
    }

    let lines = file.contents.lines().collect::<Vec<_>>();
    let mut segments = Vec::new();
    let mut body = String::new();
    for (index, line) in lines.iter().enumerate() {
        let numbered_line = format!("{} {}\n", index + 1, line);
        if wrapper_bytes + body.len() + numbered_line.len() > source_budget {
            if body.is_empty() {
                return Err(format!(
                    "Line {} of '{}' cannot fit in the {source_budget}-byte chunk budget.",
                    index + 1,
                    file.path
                ));
            }
            segments.push(format!("{header}{body}{footer}"));
            body.clear();
        }
        body.push_str(&numbered_line);
    }
    if !body.is_empty() {
        segments.push(format!("{header}{body}{footer}"));
    } else if lines.is_empty() {
        return Err(format!(
            "Empty source file '{}' cannot fit in the chunk budget.",
            file.path
        ));
    }
    Ok(segments)
}

fn report(progress: &Function, message: String) -> Result<(), String> {
    progress
        .call1(&JsValue::NULL, &JsValue::from_str(&message))
        .map(|_| ())
        .map_err(|error| {
            error
                .as_string()
                .unwrap_or_else(|| "Browser progress callback failed.".to_owned())
        })
}

fn source_file(path: &str, contents: String) -> files::SourceFile {
    let path_buf = PathBuf::from(path);
    let parent = path_buf.parent().unwrap_or_else(|| Path::new(""));
    let name = path_buf
        .file_stem()
        .and_then(|part| part.to_str())
        .unwrap_or(path)
        .to_owned();
    let extension = path_buf
        .extension()
        .and_then(|part| part.to_str())
        .map(str::to_owned);
    files::SourceFile {
        path: parent.to_path_buf(),
        name,
        extension,
        contents,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn openrouter_endpoint_uses_proxy_when_configured() {
        assert_eq!(
            openrouter_endpoint(""),
            "https://openrouter.ai/api/v1/chat/completions"
        );
        assert_eq!(
            openrouter_endpoint("http://localhost:36800/api/openrouter/chat/completions"),
            "http://localhost:36800/api/openrouter/chat/completions"
        );
    }

    #[test]
    fn single_agent_prompt_is_not_duplicated_in_the_system_context() {
        let messages = single_messages("project files".to_owned());

        assert_eq!(messages.len(), 2);
        assert_eq!(messages[0].content, "project files");
        assert_eq!(messages[1].content, NODEGRAPH_PROMPT);
    }

    #[test]
    fn applies_root_and_nested_gitignore_rules() {
        let response = filter_ignored(IgnoreRequest {
            files: [
                "app.log",
                "keep.log",
                "src/app.log",
                "src/out.log",
                "build/keep.log",
                "src/main.rs",
            ]
            .into_iter()
            .map(|path| PathInput {
                path: path.to_owned(),
            })
            .collect(),
            gitignores: vec![
                FileInput {
                    path: ".gitignore".to_owned(),
                    contents: "*.log\n/build/\n!keep.log\n".to_owned(),
                },
                FileInput {
                    path: "src/.gitignore".to_owned(),
                    contents: "!app.log\n".to_owned(),
                },
            ],
        });

        assert_eq!(
            response["ignored"],
            serde_json::json!(["app.log", "src/out.log", "build/keep.log"])
        );
    }

    #[test]
    fn refuses_to_reinclude_files_inside_an_ignored_directory() {
        let response = filter_ignored(IgnoreRequest {
            files: vec![PathInput {
                path: "build/keep.log".to_owned(),
            }],
            gitignores: vec![FileInput {
                path: ".gitignore".to_owned(),
                contents: "/build/\n!build/keep.log\n".to_owned(),
            }],
        });

        assert_eq!(response["ignored"], serde_json::json!(["build/keep.log"]));
    }

    #[test]
    fn validates_graph_against_browser_selected_files() {
        let response = validate(Request {
            source: r#"<graph type="TD">
Main["<name>Main</name><source>src/main.rs:1-1</source>"]
</graph>"#
                .to_owned(),
            files: vec![FileInput {
                path: "src/main.rs".to_owned(),
                contents: "fn main() {}\n".to_owned(),
            }],
        });

        assert!(response.get("graph").is_some());
        assert!(response.get("error").is_none());
    }

    #[test]
    fn reports_missing_browser_selected_source_files() {
        let response = validate(Request {
            source: r#"<graph type="TD">
Main["<name>Main</name><source>missing.rs:full</source>"]
</graph>"#
                .to_owned(),
            files: Vec::new(),
        });

        assert!(
            response["error"]
                .as_str()
                .unwrap()
                .contains("not present in the provided codebase")
        );
    }

    fn summary_request(context_length: usize) -> SummaryRequest {
        SummaryRequest {
            nodes: vec![
                SummaryNode {
                    id: "Current".to_owned(),
                    name: Some("Current node".to_owned()),
                    sources: vec![graph_parser::SourceReference {
                        path: "current.rs".to_owned(),
                        start: None,
                        end: None,
                    }],
                },
                SummaryNode {
                    id: "First".to_owned(),
                    name: None,
                    sources: vec![graph_parser::SourceReference {
                        path: "first.rs".to_owned(),
                        start: None,
                        end: None,
                    }],
                },
                SummaryNode {
                    id: "Second".to_owned(),
                    name: None,
                    sources: vec![graph_parser::SourceReference {
                        path: "second.rs".to_owned(),
                        start: None,
                        end: None,
                    }],
                },
            ],
            edges: vec![
                SummaryEdge {
                    from: "Current".to_owned(),
                    to: "First".to_owned(),
                },
                SummaryEdge {
                    from: "Second".to_owned(),
                    to: "Current".to_owned(),
                },
            ],
            node_id: "Current".to_owned(),
            files: vec![
                FileInput {
                    path: "current.rs".to_owned(),
                    contents: "current".to_owned(),
                },
                FileInput {
                    path: "first.rs".to_owned(),
                    contents: "first".to_owned(),
                },
                FileInput {
                    path: "second.rs".to_owned(),
                    contents: "x".repeat(1000),
                },
            ],
            model: "test/model".to_owned(),
            api_key: "test-key".to_owned(),
            api_url: String::new(),
            context_length,
            referer: "https://example.com".to_owned(),
        }
    }

    #[test]
    fn summary_context_keeps_current_node_then_fits_connected_nodes_in_graph_order() {
        let prepared = build_summary_prompt(&summary_request(400)).unwrap();

        assert!(prepared.prompt.starts_with("<Current>"));
        assert!(prepared.prompt.contains("<First>"));
        assert!(!prepared.prompt.contains("<Second>"));
        assert_eq!(prepared.included_connected_nodes, 1);
        assert_eq!(prepared.total_connected_nodes, 2);
        assert!(prepared.estimated_bytes <= prepared.context_limit_bytes);
    }

    #[test]
    fn summary_context_rejects_current_node_when_it_exceeds_byte_budget() {
        let mut request = summary_request(100);
        request.files[0].contents = "x".repeat(1000);

        let error = build_summary_prompt(&request).unwrap_err();

        assert!(error.contains("current node's source context"));
    }
}
