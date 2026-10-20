//! Embeds the graph-generation prompt that instructs the LLM to describe the
//! collected codebase using the custom graph syntax.

const NODEGRAPH_PROMPT_BYTES: &[u8] = include_bytes!("../prompts/nodegraph.txt");

pub fn nodegraph() -> &'static str {
    std::str::from_utf8(NODEGRAPH_PROMPT_BYTES)
        .expect("embedded prompt must be valid UTF-8")
        .trim_end_matches(['\r', '\n'])
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn nodegraph_prompt_is_embedded_from_prompt_file() {
        assert!(nodegraph().contains("Start with `graph TD` or `graph LR`"));
        assert!(nodegraph().contains("UpperCamelCase"));
        assert!(nodegraph().contains("no Markdown fences, preamble, or explanation"));
        assert!(nodegraph().contains("<name>Input</name><desc>Parse input</desc>"));
        assert!(nodegraph().contains("`<name>...</name>`, a `<desc>...</desc>`, or both"));
    }
}
