//! Embeds the graph-generation prompt that instructs the LLM to describe the
//! collected codebase using the custom graph syntax.

const NODEGRAPH_PROMPT_BYTES: &[u8] = include_bytes!("../prompts/nodegraph.txt");

pub fn nodegraph() -> &'static str {
    std::str::from_utf8(NODEGRAPH_PROMPT_BYTES)
        .expect("embedded prompt must be valid UTF-8")
        .trim_end_matches(['\r', '\n'])
}
