use base64::Engine;
use base64::engine::general_purpose::STANDARD;

const TEMPLATE: &str = include_str!("../public/index.html");
const CSS: &str = include_str!("../public/styles.css");
const ELK_JS: &str = include_str!("../public/vendor/elk.bundled.js");
const ELK_LICENSE: &str = include_str!("../public/vendor/elk.LICENSE");
const LIBAVOID_JS: &str = include_str!("../public/vendor/elkjs-libavoid.bundled.js");
const LIBAVOID_WASM: &[u8] = include_bytes!("../public/vendor/libavoid.wasm");
const LIBAVOID_LICENSE: &str = include_str!("../public/vendor/elkjs-libavoid.LICENSE");
const LIBAVOID_JS_LICENSE: &str = include_str!("../public/vendor/libavoid-js.LICENSE");
const HIGHLIGHT_JS: &str = include_str!("../public/vendor/highlight.min.js");
const MARKDOWN_IT_JS: &str = include_str!("../public/vendor/markdown-it.min.js");
const MARKDOWN_IT_LICENSE: &str = include_str!("../public/vendor/markdown-it.LICENSE");
const PURIFY_JS: &str = include_str!("../public/vendor/purify.min.js");
const PURIFY_LICENSE: &str = include_str!("../public/vendor/purify.LICENSE");
const PURIFY_MPL_LICENSE: &str = include_str!("../public/vendor/purify.LICENSE-MPL");

pub fn render_page_html(
    graph_details: &str,
    graph_json: &str,
    source_json: &str,
    codemap_wasm: &[u8],
    codemap_bindings_js: &str,
    system_prompt: &str,
    default_api_key: &str,
) -> String {
    let system_prompt_json = json_script_value(system_prompt);
    let split_at = default_api_key
        .char_indices()
        .nth(default_api_key.chars().count() / 2)
        .map_or(default_api_key.len(), |(index, _)| index);
    let (default_api_key_first, default_api_key_second) = default_api_key.split_at(split_at);
    let default_api_key_first_json = json_script_value(default_api_key_first);
    let default_api_key_second_json = json_script_value(default_api_key_second);

    TEMPLATE
        .replace("{{CSS}}", CSS)
        .replace("{{GRAPH_DETAILS}}", graph_details)
        .replace("{{GRAPH_JSON}}", graph_json)
        .replace("{{SOURCE_JSON}}", source_json)
        .replace("{{ELK_JS}}", ELK_JS)
        .replace("{{ELK_LICENSE}}", &escape_html(ELK_LICENSE))
        .replace("{{LIBAVOID_JS}}", LIBAVOID_JS)
        .replace("{{LIBAVOID_WASM}}", &STANDARD.encode(LIBAVOID_WASM))
        .replace("{{LIBAVOID_LICENSE}}", &escape_html(LIBAVOID_LICENSE))
        .replace("{{LIBAVOID_JS_LICENSE}}", &escape_html(LIBAVOID_JS_LICENSE))
        .replace("{{HIGHLIGHT_JS}}", HIGHLIGHT_JS)
        .replace("{{MARKDOWN_IT_JS}}", MARKDOWN_IT_JS)
        .replace(
            "{{MARKDOWN_IT_LICENSE}}",
            &escape_html_license(MARKDOWN_IT_LICENSE),
        )
        .replace("{{PURIFY_JS}}", PURIFY_JS)
        .replace("{{PURIFY_LICENSE}}", &escape_html_license(PURIFY_LICENSE))
        .replace(
            "{{PURIFY_MPL_LICENSE}}",
            &escape_html_license(PURIFY_MPL_LICENSE),
        )
        .replace("{{CODEMAP_BINDINGS_JS}}", codemap_bindings_js)
        .replace("{{CODEMAP_WASM}}", &STANDARD.encode(codemap_wasm))
        .replace("{{SYSTEM_PROMPT_JSON}}", &system_prompt_json)
        .replace(
            "{{DEFAULT_API_KEY_FIRST_JSON}}",
            &default_api_key_first_json,
        )
        .replace(
            "{{DEFAULT_API_KEY_SECOND_JSON}}",
            &default_api_key_second_json,
        )
}

fn json_script_value(value: &str) -> String {
    serde_json::to_string(value)
        .expect("script values can be serialized as JSON")
        .replace('<', "\\u003c")
}

fn escape_html(value: &str) -> String {
    value
        .replace('&', "&amp;")
        .replace('<', "&lt;")
        .replace('>', "&gt;")
        .replace('"', "&quot;")
        .replace('\'', "&#39;")
}

fn escape_html_license(value: &str) -> String {
    let normalized = value
        .lines()
        .map(str::trim_end)
        .collect::<Vec<_>>()
        .join("\n");
    escape_html(&normalized)
}
