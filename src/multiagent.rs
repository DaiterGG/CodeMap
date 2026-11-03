//! Generates oversized repository graphs from file and line chunks.

use crate::files::{SourceFile, format_source_files};
use crate::graph_parser::{self, DiagnosticKind, Graph};
use crate::step::{ChatClient, Message, Step, StepId};
use std::io;
use std::thread;

const CHUNK_CONTEXT_PERCENT: usize = 80;
const MAX_RETRIES: usize = 3;
const NODEGRAPH_PROMPT: &str = include_str!("../prompts/nodegraph.txt");
const START_PROMPT: &str = include_str!("../prompts/multiagent_start.txt");
const SEQUENTIAL_PROMPT: &str = include_str!("../prompts/multiagent_sequential.txt");
const PARALLEL_PROMPT: &str = include_str!("../prompts/multiagent_parallel.txt");
const SEGMENT_PROMPT: &str = "A file may be provided as a segment. Its displayed line numbers are the original file line numbers; preserve those numbers in source references and do not assume the segment begins at line 1.";

struct ChunkGeneration {
    response: Message,
    history: Vec<Step>,
    validation_errors: Vec<Option<String>>,
}

pub struct GeneratedGraph {
    pub graph: Graph,
    pub source: String,
    pub history: Vec<Step>,
    pub validation_errors: Vec<Option<String>>,
}

pub fn should_use_multiagent(source_files: &[SourceFile], context_length: usize) -> bool {
    format_source_files(source_files).len() + NODEGRAPH_PROMPT.len() > context_length
}

pub fn generate(
    llm: &dyn ChatClient,
    source_files: &[SourceFile],
    context_length: usize,
    ignore_big_files: bool,
) -> io::Result<GeneratedGraph> {
    generate_with_mode(llm, source_files, context_length, ignore_big_files, false)
}

pub fn generate_with_mode(
    llm: &dyn ChatClient,
    source_files: &[SourceFile],
    context_length: usize,
    ignore_big_files: bool,
    parallel: bool,
) -> io::Result<GeneratedGraph> {
    if context_length == 0 {
        return Err(io::Error::other(
            "model context size must be greater than zero",
        ));
    }

    let source_budget = context_length
        .saturating_mul(CHUNK_CONTEXT_PERCENT)
        .checked_div(100)
        .unwrap_or_default();
    let chunks = collect_chunks(source_files, source_budget, ignore_big_files)?;
    if chunks.is_empty() {
        return Err(io::Error::other(
            "no source files fit in the source chunk budget; increase the model context or include fewer files",
        ));
    }
    eprintln!(
        "multiagent: prepared all {} chunks before generation",
        chunks.len()
    );

    let chunk_generations = if parallel {
        generate_chunks_parallel(llm, source_files, &chunks, context_length)?
    } else {
        let mut generated = Vec::with_capacity(chunks.len());
        let mut previous_agent = None;
        for (index, chunk) in chunks.iter().enumerate() {
            let result = generate_chunk(
                llm,
                source_files,
                chunk,
                previous_agent.as_ref(),
                index + 1,
                chunks.len(),
                context_length,
            )?;
            previous_agent = Some(result.response.clone());
            generated.push(result);
        }
        generated
    };

    let mut history = Vec::new();
    let mut validation_errors = Vec::new();
    for generation in &chunk_generations {
        for mut step in generation.history.iter().cloned() {
            step.id = StepId(history.len());
            history.push(step);
        }
        validation_errors.extend(generation.validation_errors.iter().cloned());
    }

    let final_chunk = chunks
        .last()
        .expect("chunks are checked to be non-empty")
        .clone();
    let mut aggregation_messages = None;
    let mut response = if parallel {
        let messages = parallel_aggregation_messages(&chunk_generations);
        log_step(
            history.len() + 1,
            chunks.len() + 1,
            chunks.len() + 1,
            messages.iter().map(|message| message.content.len()).sum(),
            context_length,
        );
        let response = complete_with_retries(llm, &messages)?;
        history.push(step_from_messages(
            &messages,
            response.clone(),
            history.len(),
        ));
        aggregation_messages = Some(messages);
        response
    } else {
        chunk_generations
            .last()
            .expect("chunks are checked to be non-empty")
            .response
            .clone()
    };

    if parallel {
        validation_errors.push(
            graph_validation(&response, source_files)
                .err()
                .map(|error| {
                    format!(
                        "Final graph validation failed ({:?}):\n{}",
                        error.kind,
                        error.correction_details()
                    )
                }),
        );
    }

    for correction_attempt in 0..=MAX_RETRIES {
        let validation = graph_validation(&response, source_files);
        if validation.is_ok() {
            let graph = graph_parser::parse(&response.content)
                .map_err(|diagnostic| io::Error::other(diagnostic.correction_details()))?;
            return Ok(GeneratedGraph {
                graph,
                source: response.content,
                history,
                validation_errors,
            });
        }
        let diagnostic = validation.expect_err("the invalid final graph has a diagnostic");
        if correction_attempt == MAX_RETRIES {
            eprintln!(
                "multiagent: final graph still fails validation after {MAX_RETRIES} corrections:\n{}",
                diagnostic.correction_details()
            );
            return Err(io::Error::other(format!(
                "could not finalize the graph after {MAX_RETRIES} corrections: {}",
                diagnostic.correction_details()
            )));
        }

        let correction = if diagnostic.kind == DiagnosticKind::PartialReference {
            format!(
                "There is no more code to inspect. Remove every remaining `<partial>...</partial>` element or transform it into a complete graph node and connection. Preserve the valid graph. Return only the complete corrected graph.\n\n{}",
                diagnostic.correction_details()
            )
        } else {
            correction_for(&diagnostic)
        };
        eprintln!(
            "multiagent: final graph failed validation; requesting correction {}/{}:\n{}",
            correction_attempt + 1,
            MAX_RETRIES,
            diagnostic.correction_details()
        );
        let mut messages = if let Some(aggregation_messages) = &aggregation_messages {
            let mut messages = aggregation_messages.clone();
            messages.push(format_previous_agent(&response));
            messages
        } else {
            base_messages(&final_chunk, Some(&response))
        };
        messages.push(Message::user(correction));
        log_step(
            history.len() + 1,
            chunks.len(),
            chunks.len(),
            final_chunk.len(),
            context_length,
        );
        let corrected = complete_with_retries(llm, &messages)?;
        let validation = graph_validation(&corrected, source_files);
        history.push(step_from_messages(
            &messages,
            corrected.clone(),
            history.len(),
        ));
        validation_errors.push(validation.err().map(|diagnostic| {
            format!(
                "Final graph validation failed ({:?}):\n{}",
                diagnostic.kind,
                diagnostic.correction_details()
            )
        }));
        response = corrected;
    }

    Err(io::Error::other("final graph validation did not complete"))
}

fn generate_chunks_parallel(
    llm: &dyn ChatClient,
    source_files: &[SourceFile],
    chunks: &[String],
    context_length: usize,
) -> io::Result<Vec<ChunkGeneration>> {
    thread::scope(|scope| {
        let handles = chunks
            .iter()
            .enumerate()
            .map(|(index, chunk)| {
                scope.spawn(move || {
                    generate_chunk(
                        llm,
                        source_files,
                        chunk,
                        None,
                        index + 1,
                        chunks.len(),
                        context_length,
                    )
                })
            })
            .collect::<Vec<_>>();

        let mut results = Vec::with_capacity(handles.len());
        let mut first_error = None;
        for handle in handles {
            match handle.join() {
                Ok(Ok(result)) => results.push(Some(result)),
                Ok(Err(error)) => {
                    if first_error.is_none() {
                        first_error = Some(error);
                    }
                    results.push(None);
                }
                Err(_) => {
                    if first_error.is_none() {
                        first_error = Some(io::Error::other(
                            "parallel multi-agent chunk worker panicked",
                        ));
                    }
                    results.push(None);
                }
            }
        }

        if let Some(error) = first_error {
            return Err(error);
        }
        Ok(results
            .into_iter()
            .map(|result| result.expect("successful chunk results are present"))
            .collect())
    })
}

fn generate_chunk(
    llm: &dyn ChatClient,
    source_files: &[SourceFile],
    chunk_text: &str,
    previous_agent: Option<&Message>,
    chunk_index: usize,
    total_chunks: usize,
    context_length: usize,
) -> io::Result<ChunkGeneration> {
    eprintln!("multiagent: processing chunk {chunk_index}/{total_chunks}");
    let mut history = Vec::new();
    let mut validation_errors = Vec::new();
    let mut last_attempt: Option<(Message, String)> = None;

    for correction_attempt in 0..=MAX_RETRIES {
        let mut messages = base_messages(chunk_text, previous_agent);
        if let Some((failed_response, correction)) = &last_attempt {
            messages.push(format_previous_agent(failed_response));
            messages.push(Message::user(correction));
        }
        log_step(
            chunk_index,
            chunk_index,
            total_chunks,
            chunk_text.len(),
            context_length,
        );
        let response = complete_with_retries(llm, &messages)?;
        let validation = graph_parser::validate_with_sources_in_chunk(
            &response.content,
            source_files,
            Some(chunk_text),
        );
        history.push(step_from_messages(
            &messages,
            response.clone(),
            history.len(),
        ));

        match validation {
            Ok(()) => {
                validation_errors.push(None);
                return Ok(ChunkGeneration {
                    response,
                    history,
                    validation_errors,
                });
            }
            Err(diagnostic) => {
                let error = format!(
                    "Graph validation failed ({:?}):\n{}",
                    diagnostic.kind,
                    diagnostic.correction_details()
                );
                validation_errors.push(Some(error.clone()));
                if correction_attempt == MAX_RETRIES {
                    eprintln!(
                        "multiagent: chunk {chunk_index}/{total_chunks} still fails graph validation after {MAX_RETRIES} corrections:\n{error}"
                    );
                    return Err(io::Error::other(format!(
                        "could not generate a valid graph after {MAX_RETRIES} corrections: {error}"
                    )));
                }
                eprintln!(
                    "multiagent: chunk {chunk_index}/{total_chunks} failed graph validation; retrying the same chunk with correction {}/{}:\n{error}",
                    correction_attempt + 1,
                    MAX_RETRIES
                );
                last_attempt = Some((response, correction_for(&diagnostic)));
            }
        }
    }

    Err(io::Error::other(
        "chunk generation ended without a valid graph",
    ))
}

fn parallel_aggregation_messages(generations: &[ChunkGeneration]) -> Vec<Message> {
    let mut messages = vec![
        Message::system(NODEGRAPH_PROMPT),
        Message::user(SEGMENT_PROMPT),
        Message::user(START_PROMPT),
        Message::user(PARALLEL_PROMPT),
    ];
    let mut dump = String::new();
    for (index, generation) in generations.iter().enumerate() {
        let thought = generation
            .response
            .reasoning
            .as_deref()
            .unwrap_or("(no reasoning supplied)");
        dump.push_str(&format!(
            "Agent {} thought process:\n{}\n\nAgent {} graph output:\n{}\n\n",
            index + 1,
            thought,
            index + 1,
            generation.response.content
        ));
    }
    messages.push(Message::user(dump));
    messages
}

fn graph_validation(
    response: &Message,
    source_files: &[SourceFile],
) -> Result<(), graph_parser::GraphDiagnostic> {
    graph_parser::validate_with_sources(&response.content, source_files)
        .and_then(|_| graph_parser::validate_no_partials(&response.content))
}

fn log_step(
    step: usize,
    chunk: usize,
    total_chunks: usize,
    chunk_bytes: usize,
    context_length: usize,
) {
    eprintln!(
        "multiagent step {step}: chunk {chunk}/{total_chunks} is {chunk_bytes} bytes; model context window is {context_length} tokens"
    );
}

fn collect_chunks(
    source_files: &[SourceFile],
    source_budget: usize,
    ignore_big_files: bool,
) -> io::Result<Vec<String>> {
    let mut units = Vec::new();
    for file in source_files {
        let formatted_file = format_source_files(std::slice::from_ref(file));
        if formatted_file.len() <= source_budget {
            units.push(formatted_file);
        } else if ignore_big_files {
            eprintln!(
                "multiagent: ignoring oversized file '{}' ({} bytes exceeds the {source_budget}-byte source chunk budget)",
                source_file_path(file),
                formatted_file.len()
            );
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

fn split_file(file: &SourceFile, source_budget: usize) -> io::Result<Vec<String>> {
    let relative_path = source_file_path(file).replace('\\', "/");
    let header = format!("``` {relative_path}\n");
    let footer = "```\n\n";
    let wrapper_bytes = header.len() + footer.len();
    let lines = file.contents.lines().collect::<Vec<_>>();
    if wrapper_bytes > source_budget {
        return Err(io::Error::other(format!(
            "source file '{}' cannot fit in the {source_budget}-byte source chunk budget, even as a line segment",
            source_file_path(file)
        )));
    }

    let mut segments = Vec::new();
    let mut body = String::new();
    for (line_index, line) in lines.iter().enumerate() {
        let line_number = line_index + 1;
        let numbered_line = format!("{line_number} {line}\n");
        if wrapper_bytes + body.len() + numbered_line.len() > source_budget {
            if body.is_empty() {
                return Err(io::Error::other(format!(
                    "line {line_number} of source file '{}' cannot fit in the {source_budget}-byte source chunk budget",
                    source_file_path(file)
                )));
            }
            segments.push(format!("{header}{body}{footer}"));
            body.clear();
        }
        body.push_str(&numbered_line);
    }
    if !body.is_empty() {
        segments.push(format!("{header}{body}{footer}"));
    } else if lines.is_empty() {
        return Err(io::Error::other(format!(
            "source file '{}' cannot fit in the {source_budget}-byte source chunk budget",
            source_file_path(file)
        )));
    }

    if segments.len() > 1 {
        eprintln!(
            "multiagent: splitting oversized file '{}' into {} line segments",
            source_file_path(file),
            segments.len()
        );
    }
    Ok(segments)
}

fn base_messages(chunk_text: &str, previous_agent: Option<&Message>) -> Vec<Message> {
    let mut messages = vec![
        Message::system(chunk_text),
        Message::user(NODEGRAPH_PROMPT),
        Message::user(SEGMENT_PROMPT),
        Message::user(START_PROMPT),
    ];
    if let Some(previous_agent) = previous_agent {
        messages.push(format_previous_agent(previous_agent));
        messages.push(Message::user(SEQUENTIAL_PROMPT));
    }
    messages
}

fn format_previous_agent(message: &Message) -> Message {
    let thought = message
        .reasoning
        .as_deref()
        .unwrap_or("(no reasoning supplied)");
    Message {
        role: crate::step::Role::Assistant,
        content: format!(
            "Previous agent thought process:\n{thought}\n\nPrevious agent graph output:\n{}",
            message.content
        ),
        reasoning: message.reasoning.clone(),
    }
}

#[cfg(test)]
fn message_bytes(messages: &[Message]) -> usize {
    messages.iter().map(|message| message.content.len()).sum()
}

fn complete_with_retries(llm: &dyn ChatClient, messages: &[Message]) -> io::Result<Message> {
    let mut last_error = None;
    for attempt in 0..=MAX_RETRIES {
        match llm.complete(messages) {
            Ok(response) => return Ok(response),
            Err(error) if attempt == MAX_RETRIES => last_error = Some(error.to_string()),
            Err(error) => {
                eprintln!("retry: model request failed: {error}");
            }
        }
    }
    Err(io::Error::other(format!(
        "model request failed after {MAX_RETRIES} retries: {}",
        last_error.unwrap_or_else(|| "unknown model error".to_owned())
    )))
}

fn correction_for(diagnostic: &graph_parser::GraphDiagnostic) -> String {
    let prompt = match diagnostic.kind {
        DiagnosticKind::Syntax => include_str!("../prompts/syntax_error.txt"),
        DiagnosticKind::RedundantConnection => {
            include_str!("../prompts/redundant_connection.txt")
        }
        DiagnosticKind::SourceReference => {
            include_str!("../prompts/source_reference_error.txt")
        }
        DiagnosticKind::PartialReference => {
            "Correct the partial connection using the exact invocation line in the current chunk. Preserve valid graph content and return only the complete corrected graph."
        }
    };
    format!("{prompt}\n\n{}", diagnostic.correction_details())
}

fn step_from_messages(messages: &[Message], response: Message, index: usize) -> Step {
    Step {
        id: StepId(index),
        input: messages.iter().skip(1).cloned().collect(),
        response,
    }
}

fn source_file_path(file: &SourceFile) -> String {
    let mut filename = file.name.clone();
    if let Some(extension) = &file.extension {
        filename.push('.');
        filename.push_str(extension);
    }
    file.path.join(filename).display().to_string()
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::error::Error;
    use std::path::PathBuf;
    use std::sync::Mutex;
    use std::sync::atomic::{AtomicUsize, Ordering};
    use std::time::Duration;

    struct RecordingClient {
        responses: Mutex<Vec<Message>>,
        requests: Mutex<Vec<Vec<Message>>>,
    }

    impl ChatClient for RecordingClient {
        fn complete(&self, messages: &[Message]) -> Result<Message, Box<dyn Error + Send + Sync>> {
            self.requests.lock().unwrap().push(messages.to_vec());
            Ok(self.responses.lock().unwrap().remove(0))
        }
    }

    struct ParallelClient {
        active: AtomicUsize,
        max_active: AtomicUsize,
        requests: Mutex<Vec<Vec<Message>>>,
    }

    impl ChatClient for ParallelClient {
        fn complete(&self, messages: &[Message]) -> Result<Message, Box<dyn Error + Send + Sync>> {
            self.requests.lock().unwrap().push(messages.to_vec());
            if messages
                .iter()
                .any(|message| message.content == PARALLEL_PROMPT)
            {
                return Ok(assistant(
                    r#"<graph type="TD">
Main["<name>Main</name><source>main.rs:full</source>"]
Second["<name>Second</name><source>second.rs:full</source>"]
</graph>"#,
                ));
            }

            let active = self.active.fetch_add(1, Ordering::SeqCst) + 1;
            self.max_active.fetch_max(active, Ordering::SeqCst);
            thread::sleep(Duration::from_millis(40));
            self.active.fetch_sub(1, Ordering::SeqCst);

            let graph = if messages[0].content.contains("main.rs") {
                r#"<graph type="TD">
Main["<name>Main</name><source>main.rs:full</source>"]
</graph>"#
            } else {
                r#"<graph type="TD">
Second["<name>Second</name><source>second.rs:full</source>"]
</graph>"#
            };
            Ok(assistant(graph))
        }
    }

    fn file(name: &str, contents: &str) -> SourceFile {
        SourceFile {
            path: PathBuf::new(),
            name: name.to_owned(),
            extension: Some("rs".to_owned()),
            contents: contents.to_owned(),
        }
    }

    fn assistant(content: &str) -> Message {
        Message {
            role: crate::step::Role::Assistant,
            content: content.to_owned(),
            reasoning: Some("reviewed this chunk".to_owned()),
        }
    }

    #[test]
    fn detects_when_the_repository_exceeds_the_model_context() {
        let files = [file("main", "short")];
        let full_context = format_source_files(&files).len() + NODEGRAPH_PROMPT.len();
        assert!(!should_use_multiagent(&files, full_context));
        assert!(should_use_multiagent(&files, full_context - 1));
    }

    #[test]
    fn chunks_only_at_file_boundaries_and_budgets_source_content_only() {
        let files = [file("a", "1234"), file("b", "5678")];
        let budget = format_source_files(std::slice::from_ref(&files[0])).len();
        let chunks = collect_chunks(&files, budget, false).unwrap();
        let text = &chunks[0];
        let request = base_messages(&text, None);

        assert_eq!(*text, format_source_files(std::slice::from_ref(&files[0])));
        assert_eq!(chunks.len(), 2);
        assert!(message_bytes(&request) > budget);
    }

    #[test]
    fn splits_large_files_at_line_boundaries_with_original_line_numbers() {
        let files = [file("large", "first\nsecond\nthird")];
        let source_budget = "``` large.rs\n1 first\n2 second\n```\n\n".len();
        let chunks = collect_chunks(&files, source_budget, false).unwrap();

        assert_eq!(chunks.len(), 2);
        assert!(chunks.iter().all(|chunk| chunk.len() <= source_budget));
        assert!(chunks[0].contains("1 first\n2 second\n"));
        assert!(chunks[1].contains("3 third\n"));
    }

    #[test]
    fn sends_line_segments_of_one_large_file_in_separate_requests() {
        let files = [file(
            "large",
            &format!("{}\n{}\n{}", "a".repeat(20), "b".repeat(20), "c".repeat(20)),
        )];
        let graph = r#"<graph type="TD">
Large["<name>Large</name><source>large.rs:full</source>"]
</graph>"#;
        let llm = RecordingClient {
            responses: Mutex::new(vec![assistant(graph), assistant(graph)]),
            requests: Mutex::new(Vec::new()),
        };

        generate(&llm, &files, 100, false).unwrap();
        let requests = llm.requests.lock().unwrap();

        assert_eq!(requests.len(), 2);
        assert!(requests[0][0].content.contains("1 "));
        assert!(requests[0][0].content.contains("2 "));
        assert!(!requests[0][0].content.contains("3 "));
        assert!(requests[1][0].content.contains("3 "));
    }

    #[test]
    fn ignore_big_files_skips_files_that_would_be_split() {
        let files = [file("large", "first\nsecond\nthird"), file("small", "ok")];
        let source_budget = format_source_files(std::slice::from_ref(&files[1])).len();
        let chunks = collect_chunks(&files, source_budget, true).unwrap();

        assert_eq!(chunks.len(), 1);
        assert!(chunks[0].contains("small.rs"));
        assert!(!chunks[0].contains("large.rs"));
    }

    #[test]
    fn rejects_a_line_that_cannot_fit_in_a_source_chunk() {
        let files = [file("large", "a long file")];
        let source_budget = "``` large.rs\n".len() + "```\n\n".len() + "1 x\n".len();
        let error = collect_chunks(&files, source_budget, false).unwrap_err();
        assert!(error.to_string().contains("line 1"));
    }

    #[test]
    fn later_requests_include_previous_thought_output_and_sequential_prompt() {
        let previous = Message {
            role: crate::step::Role::Assistant,
            content: "previous graph".to_owned(),
            reasoning: Some("previous thought".to_owned()),
        };
        let messages = base_messages("second chunk", Some(&previous));

        assert_eq!(messages[0].content, "second chunk");
        assert_eq!(messages[1].content, NODEGRAPH_PROMPT);
        assert_eq!(messages[2].content, SEGMENT_PROMPT);
        assert_eq!(messages[3].content, START_PROMPT);
        assert!(messages[4].content.contains("previous thought"));
        assert!(messages[4].content.contains("previous graph"));
        assert_eq!(messages[5].content, SEQUENTIAL_PROMPT);
    }

    #[test]
    fn generates_oversized_repositories_in_sequential_file_chunks() {
        let files = [
            file("main", &"a".repeat(3000)),
            file("second", &"b".repeat(3000)),
            file("third", &"c".repeat(3000)),
        ];
        let context_length = format_source_files(&files).len() * 100 / CHUNK_CONTEXT_PERCENT - 1;
        let graph = r#"<graph type="TD">
Main["<name>Main</name><source>main.rs:full</source>"]
</graph>"#;
        let llm = RecordingClient {
            responses: Mutex::new(
                (0..10)
                    .map(|_| Message {
                        role: crate::step::Role::Assistant,
                        content: graph.to_owned(),
                        reasoning: Some("reviewed this chunk".to_owned()),
                    })
                    .collect(),
            ),
            requests: Mutex::new(Vec::new()),
        };

        assert!(should_use_multiagent(&files, context_length));
        let generated = generate(&llm, &files, context_length, false).unwrap();
        let requests = llm.requests.lock().unwrap();

        assert!(requests.len() > 1);
        assert_eq!(generated.history.len(), requests.len());
        assert_eq!(generated.graph.nodes.len(), 1);
        assert!(requests[1][0].content.contains("third.rs"));
        assert!(requests[1][4].content.contains("reviewed this chunk"));
        assert!(requests[1][4].content.contains(graph));
        assert_eq!(requests[1][5].content, SEQUENTIAL_PROMPT);
    }

    #[test]
    fn generates_chunks_concurrently_then_aggregates_all_agent_outputs() {
        let files = [
            file("main", &"a".repeat(1500)),
            file("second", &"b".repeat(1500)),
        ];
        let largest_file = files
            .iter()
            .map(|file| format_source_files(std::slice::from_ref(file)).len())
            .max()
            .unwrap();
        let context_length = largest_file * 100 / CHUNK_CONTEXT_PERCENT + 10;
        let llm = ParallelClient {
            active: AtomicUsize::new(0),
            max_active: AtomicUsize::new(0),
            requests: Mutex::new(Vec::new()),
        };

        let generated = generate_with_mode(&llm, &files, context_length, false, true).unwrap();
        let requests = llm.requests.lock().unwrap();
        let chunk_requests = requests
            .iter()
            .filter(|messages| {
                !messages
                    .iter()
                    .any(|message| message.content == PARALLEL_PROMPT)
            })
            .collect::<Vec<_>>();
        let aggregation = requests
            .iter()
            .find(|messages| {
                messages
                    .iter()
                    .any(|message| message.content == PARALLEL_PROMPT)
            })
            .unwrap();

        assert_eq!(chunk_requests.len(), 2);
        assert_eq!(llm.max_active.load(Ordering::SeqCst), 2);
        assert_eq!(generated.validation_errors.len(), generated.history.len());
        assert!(
            aggregation
                .iter()
                .any(|message| message.content == START_PROMPT)
        );
        assert!(
            aggregation
                .iter()
                .any(|message| message.content == PARALLEL_PROMPT)
        );
        assert!(chunk_requests.iter().all(|messages| messages.len() == 4));
        assert!(chunk_requests.iter().all(|messages| {
            messages
                .iter()
                .all(|message| message.content != SEQUENTIAL_PROMPT)
        }));
        let agent_dump = aggregation.last().unwrap();
        assert!(agent_dump.content.contains("reviewed this chunk"));
        assert!(agent_dump.content.contains("Main["));
        assert!(agent_dump.content.contains("Second["));
        assert_eq!(generated.history.len(), 3);
        assert_eq!(generated.graph.nodes.len(), 2);
        assert!(generated.graph.partials.is_empty());
    }

    #[test]
    fn asks_the_last_agent_to_resolve_partials_when_no_code_remains() {
        let files = [
            file("main", &"call()\n".repeat(300)),
            file("second", &"call()\n".repeat(300)),
            file("third", &"call()\n".repeat(300)),
            file("fourth", &"call()\n".repeat(300)),
        ];
        let context_length = format_source_files(&files).len() * 100 / CHUNK_CONTEXT_PERCENT - 1;
        let graph = r#"<graph type="TD">
Main["<name>Main</name><source>main.rs:full</source>"]
</graph>"#;
        let partial_graph = r#"<graph type="TD">
Main["<name>Main</name><source>main.rs:full</source>"] --> <partial>1 call()</partial>
</graph>"#;

        let chunk_count =
            collect_chunks(&files, context_length * CHUNK_CONTEXT_PERCENT / 100, false)
                .unwrap()
                .len();
        assert!(chunk_count > 1);

        let mut responses = (0..chunk_count - 1)
            .map(|_| assistant(graph))
            .collect::<Vec<_>>();
        responses.push(assistant(partial_graph));
        responses.push(assistant(graph));
        let llm = RecordingClient {
            responses: Mutex::new(responses),
            requests: Mutex::new(Vec::new()),
        };

        let generated = generate(&llm, &files, context_length, false).unwrap();
        let requests = llm.requests.lock().unwrap();

        assert!(generated.graph.partials.is_empty());
        assert_eq!(requests.len(), chunk_count + 1);
        assert!(
            requests
                .last()
                .unwrap()
                .last()
                .unwrap()
                .content
                .contains("There is no more code to inspect")
        );
    }
}
