use globset::{GlobBuilder, GlobSet, GlobSetBuilder};
use regex::RegexBuilder;
use serde::Serialize;
use std::path::{Component, Path};

const HELP: &str = include_str!("../../prompts/tools/rg/help.txt");

#[derive(Debug, PartialEq, Eq, Serialize)]
#[serde(untagged)]
pub(super) enum SearchResult {
    Output { output: String },
    Error { error: String, help: &'static str },
}

#[derive(Default)]
struct Options {
    ignore_case: bool,
    invert_match: bool,
    line_numbers: bool,
    files_with_matches: bool,
    files_without_match: bool,
    count: bool,
    only_matching: bool,
    fixed_strings: bool,
    word_regexp: bool,
    line_regexp: bool,
    list_files: bool,
    hidden: bool,
    after_context: usize,
    before_context: usize,
    globs: Vec<(bool, String)>,
    paths: Vec<String>,
}

struct MatchLine {
    number: usize,
    matches: Vec<String>,
}

pub(super) fn search(command: &str, files: &[super::FileInput]) -> SearchResult {
    match run(command, files) {
        Ok(output) => SearchResult::Output { output },
        Err(error) => SearchResult::Error { error, help: HELP },
    }
}

fn run(command: &str, files: &[super::FileInput]) -> Result<String, String> {
    let tokens = tokenize(command)?;
    let Some(command_name) = tokens.first() else {
        return Err("Missing rg command.".to_owned());
    };
    if !matches!(command_name.as_str(), "rg" | "ripgrep") {
        return Err(format!("Expected an rg command, got '{command_name}'."));
    }
    let tokens = &tokens[1..];

    if tokens
        .iter()
        .any(|token| token == "--help" || token == "-h")
    {
        return Ok(HELP.to_owned());
    }

    let (pattern, options) = parse_options(tokens)?;
    if files.is_empty() {
        return Err("No source files are currently loaded.".to_owned());
    }
    if options.only_matching && (options.before_context > 0 || options.after_context > 0) {
        return Err("--only-matching cannot be combined with context options.".to_owned());
    }
    if options.only_matching && options.invert_match {
        return Err("--only-matching cannot be combined with --invert-match.".to_owned());
    }
    if options.count && (options.files_with_matches || options.files_without_match) {
        return Err("--count cannot be combined with file-listing options.".to_owned());
    }

    let regex_pattern = if options.fixed_strings {
        regex::escape(&pattern)
    } else {
        pattern
    };
    let regex_pattern = if options.word_regexp {
        format!(r"\b(?:{regex_pattern})\b")
    } else if options.line_regexp {
        format!(r"^(?:{regex_pattern})$")
    } else {
        regex_pattern
    };
    let regex = RegexBuilder::new(&regex_pattern)
        .case_insensitive(options.ignore_case)
        .build()
        .map_err(|error| format!("Invalid regular expression: {error}"))?;

    let glob_sets = build_glob_sets(&options.globs)?;
    if !options.paths.is_empty()
        && !files
            .iter()
            .any(|file| path_is_selected(&file.path, &options.paths))
    {
        return Err("No loaded files matched the requested path.".to_owned());
    }
    let selected = files
        .iter()
        .filter(|file| path_is_selected(&file.path, &options.paths))
        .filter(|file| options.hidden || !is_hidden(&file.path))
        .filter(|file| glob_matches(&file.path, &glob_sets))
        .collect::<Vec<_>>();
    if options.list_files {
        return Ok(selected
            .iter()
            .map(|file| format!("{}\n", file.path))
            .collect());
    }

    let mut output = String::new();
    for file in selected {
        let matches = file
            .contents
            .lines()
            .enumerate()
            .filter_map(|(index, text)| {
                let found = regex
                    .find_iter(text)
                    .map(|capture| capture.as_str().to_owned())
                    .collect::<Vec<_>>();
                let is_match = !found.is_empty();
                if is_match != options.invert_match {
                    Some(MatchLine {
                        number: index + 1,
                        matches: found,
                    })
                } else {
                    None
                }
            })
            .collect::<Vec<_>>();

        if options.files_with_matches && !matches.is_empty() {
            output.push_str(&format!("{}\n", file.path));
            continue;
        }
        if options.files_without_match && matches.is_empty() {
            output.push_str(&format!("{}\n", file.path));
            continue;
        }
        if options.count {
            output.push_str(&format!("{}:{}\n", file.path, matches.len()));
            continue;
        }
        if options.only_matching {
            for matched_line in &matches {
                for matched in &matched_line.matches {
                    output.push_str(&format!(
                        "{}:{}:{}\n",
                        file.path, matched_line.number, matched
                    ));
                }
            }
            continue;
        }

        append_matches(&mut output, &file.path, &file.contents, &matches, &options);
    }
    Ok(output)
}

fn parse_options(tokens: &[String]) -> Result<(String, Options), String> {
    let mut options = Options {
        line_numbers: true,
        ..Options::default()
    };
    let mut pattern = None;
    let mut after_options = false;
    let mut index = 0;
    while index < tokens.len() {
        let token = &tokens[index];
        if after_options {
            if options.list_files {
                options.paths.push(token.clone());
            } else if pattern.is_none() {
                pattern = Some(token.clone());
            } else {
                options.paths.push(token.clone());
            }
            index += 1;
            continue;
        }
        if token == "--" {
            after_options = true;
            index += 1;
            continue;
        }
        if token == "--ignore-case" || token == "-i" {
            options.ignore_case = true;
        } else if token == "--case-sensitive" || token == "-s" {
            options.ignore_case = false;
        } else if token == "--invert-match" || token == "-v" {
            options.invert_match = true;
        } else if token == "--line-number" || token == "-n" {
            options.line_numbers = true;
        } else if token == "--no-line-number" || token == "-N" {
            options.line_numbers = false;
        } else if token == "--files-with-matches" || token == "-l" {
            options.files_with_matches = true;
        } else if token == "--files-without-match" {
            options.files_without_match = true;
        } else if token == "--count" || token == "-c" {
            options.count = true;
        } else if token == "--only-matching" || token == "-o" {
            options.only_matching = true;
        } else if token == "--fixed-strings" || token == "-F" {
            options.fixed_strings = true;
        } else if token == "--word-regexp" || token == "-w" {
            options.word_regexp = true;
        } else if token == "--line-regexp" || token == "-x" {
            options.line_regexp = true;
        } else if token == "--hidden" {
            options.hidden = true;
        } else if token == "--no-hidden" {
            options.hidden = false;
        } else if token == "--files" || token == "-f" {
            options.list_files = true;
        } else if token == "--no-ignore" || token == "--no-ignore-vcs" {
            // The browser API searches the files CodeMap has already loaded.
        } else if let Some(value) = option_value(token, "--glob", "-g") {
            options.globs.push((false, value.to_owned()));
        } else if let Some(value) = option_value(token, "--iglob", "") {
            options.globs.push((true, value.to_owned()));
        } else if let Some(value) = option_value(token, "--after-context", "-A") {
            options.after_context = parse_usize_option(value, "--after-context")?;
        } else if let Some(value) = option_value(token, "--before-context", "-B") {
            options.before_context = parse_usize_option(value, "--before-context")?;
        } else if let Some(value) = option_value(token, "--context", "-C") {
            let context = parse_usize_option(value, "--context")?;
            options.before_context = context;
            options.after_context = context;
        } else if matches!(
            token.as_str(),
            "-A" | "-B"
                | "-C"
                | "-g"
                | "--after-context"
                | "--before-context"
                | "--context"
                | "--glob"
                | "--iglob"
        ) {
            index += 1;
            let value = tokens
                .get(index)
                .ok_or_else(|| format!("Option {token} requires a value."))?;
            match token.as_str() {
                "-A" | "--after-context" => {
                    options.after_context = parse_usize_option(value, token)?
                }
                "-B" | "--before-context" => {
                    options.before_context = parse_usize_option(value, token)?
                }
                "-C" | "--context" => {
                    let context = parse_usize_option(value, token)?;
                    options.before_context = context;
                    options.after_context = context;
                }
                "-g" | "--glob" => options.globs.push((false, value.clone())),
                "--iglob" => options.globs.push((true, value.clone())),
                _ => unreachable!(),
            }
        } else if token.starts_with('-') {
            return Err(format!("Unsupported option: {token}"));
        } else if options.list_files {
            options.paths.push(token.clone());
        } else if pattern.is_none() {
            pattern = Some(token.clone());
        } else {
            options.paths.push(token.clone());
        }
        index += 1;
    }
    let pattern = if options.list_files {
        if pattern.is_some() {
            return Err("--files does not take a search pattern.".to_owned());
        }
        String::new()
    } else {
        pattern.ok_or_else(|| "Missing search pattern.".to_owned())?
    };
    if !options.list_files && pattern.is_empty() {
        return Err("The search pattern cannot be empty.".to_owned());
    }
    for path in &options.paths {
        validate_repo_path(path)?;
    }
    Ok((pattern, options))
}

fn option_value<'a>(token: &'a str, long: &str, short: &str) -> Option<&'a str> {
    if let Some(value) = token.strip_prefix(&format!("{long}=")) {
        return Some(value);
    }
    if !short.is_empty() {
        if let Some(value) = token.strip_prefix(short).filter(|value| !value.is_empty()) {
            return Some(value);
        }
    }
    None
}

fn parse_usize_option(value: &str, option: &str) -> Result<usize, String> {
    value
        .parse()
        .map_err(|_| format!("{option} expects a non-negative integer, got '{value}'."))
}

fn build_glob_sets(globs: &[(bool, String)]) -> Result<Vec<(bool, GlobSet)>, String> {
    let mut groups = Vec::new();
    for (case_insensitive, pattern) in globs {
        let (include, pattern) = pattern
            .strip_prefix('!')
            .map_or((true, pattern.as_str()), |pattern| (false, pattern));
        if pattern.is_empty() {
            return Err("Glob patterns cannot be empty.".to_owned());
        }
        let glob = GlobBuilder::new(pattern)
            .case_insensitive(*case_insensitive)
            .literal_separator(true)
            .build()
            .map_err(|error| format!("Invalid glob '{pattern}': {error}"))?;
        let mut builder = GlobSetBuilder::new();
        builder.add(glob);
        groups.push((include, builder.build().map_err(|error| error.to_string())?));
    }
    Ok(groups)
}

fn glob_matches(path: &str, globs: &[(bool, GlobSet)]) -> bool {
    if globs.is_empty() {
        return true;
    }
    let mut has_include = false;
    let mut included = false;
    for (include, glob) in globs {
        if *include {
            has_include = true;
        }
        let candidate =
            glob.is_match(path) || glob.is_match(path.rsplit('/').next().unwrap_or(path));
        if candidate {
            if *include {
                included = true;
            } else {
                return false;
            }
        }
    }
    !has_include || included
}

fn path_is_selected(path: &str, requested_paths: &[String]) -> bool {
    requested_paths.is_empty()
        || requested_paths.iter().any(|requested| {
            let normalized = requested.replace('\\', "/");
            let requested = normalized.trim_end_matches('/');
            requested.is_empty()
                || requested == "."
                || path == requested
                || path
                    .strip_prefix(requested)
                    .is_some_and(|rest| rest.starts_with('/'))
        })
}

fn validate_repo_path(path: &str) -> Result<(), String> {
    let normalized = path.replace('\\', "/");
    if normalized == "." {
        return Ok(());
    }
    let path = Path::new(&normalized);
    if path.is_absolute()
        || normalized
            .split('/')
            .next()
            .is_some_and(|component| component.ends_with(':'))
        || path
            .components()
            .any(|component| !matches!(component, Component::Normal(_)))
    {
        return Err(format!(
            "Path must stay within the loaded repository: {normalized}"
        ));
    }
    Ok(())
}

fn is_hidden(path: &str) -> bool {
    path.split('/').any(|component| component.starts_with('.'))
}

fn append_matches(
    output: &mut String,
    path: &str,
    contents: &str,
    matches: &[MatchLine],
    options: &Options,
) {
    if matches.is_empty() {
        return;
    }
    let mut ranges = matches
        .iter()
        .map(|matched| {
            matched.number.saturating_sub(options.before_context)
                ..=matched.number.saturating_add(options.after_context)
        })
        .collect::<Vec<_>>();
    ranges.sort_by_key(|range| *range.start());
    let mut merged: Vec<(usize, usize)> = Vec::new();
    for range in ranges {
        let start = *range.start();
        let end = *range.end();
        if let Some(last) = merged.last_mut().filter(|last| start <= last.1 + 1) {
            last.1 = last.1.max(end);
        } else {
            merged.push((start, end));
        }
    }
    let matching_numbers = matches
        .iter()
        .map(|matched| matched.number)
        .collect::<std::collections::HashSet<_>>();
    let source_lines = contents.lines().collect::<Vec<_>>();
    for (range_index, (start, end)) in merged.iter().enumerate() {
        if range_index > 0 {
            output.push_str("--\n");
        }
        for number in *start..=*end.min(&source_lines.len()) {
            if let Some(text) = source_lines.get(number - 1) {
                append_output_line(
                    output,
                    path,
                    number,
                    text,
                    matching_numbers.contains(&number),
                    options,
                );
            }
        }
    }
}

fn append_output_line(
    output: &mut String,
    path: &str,
    number: usize,
    text: &str,
    is_match: bool,
    options: &Options,
) {
    output.push_str(path);
    output.push(if is_match { ':' } else { '-' });
    if options.line_numbers {
        output.push_str(&number.to_string());
        output.push(if is_match { ':' } else { '-' });
    }
    output.push_str(text);
    output.push('\n');
}

fn tokenize(command: &str) -> Result<Vec<String>, String> {
    let mut tokens = Vec::new();
    let mut token = String::new();
    let mut quote = None;
    let mut escaped = false;
    let mut started = false;
    let mut characters = command.chars().peekable();
    while let Some(character) = characters.next() {
        if escaped {
            token.push(character);
            escaped = false;
            continue;
        }
        match (quote, character) {
            (Some(current), '\\') if characters.peek() == Some(&current) => escaped = true,
            (Some(current), character) if current == character => quote = None,
            (Some(_), character) => token.push(character),
            (None, '\'' | '"') => {
                quote = Some(character);
                started = true;
            }
            (None, character) if character.is_whitespace() => {
                if started {
                    tokens.push(std::mem::take(&mut token));
                    started = false;
                }
            }
            (None, character) => {
                token.push(character);
                started = true;
            }
        }
    }
    if escaped {
        token.push('\\');
    }
    if quote.is_some() {
        return Err("Unclosed quote in command.".to_owned());
    }
    if started {
        tokens.push(token);
    }
    Ok(tokens)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn files() -> Vec<super::super::FileInput> {
        vec![
            super::super::FileInput {
                path: "src/main.rs".to_owned(),
                contents: "fn main() {\n    println!(\"Hello\");\n}\n".to_owned(),
            },
            super::super::FileInput {
                path: "README.md".to_owned(),
                contents: "hello world\n".to_owned(),
            },
            super::super::FileInput {
                path: ".hidden/config".to_owned(),
                contents: "hello\n".to_owned(),
            },
        ]
    }

    #[test]
    fn searches_loaded_files_with_path_and_line_numbers() {
        assert_eq!(
            search("rg -i hello", &files()),
            SearchResult::Output {
                output: "src/main.rs:2:    println!(\"Hello\");\nREADME.md:1:hello world\n"
                    .to_owned()
            }
        );
    }

    #[test]
    fn supports_globs_context_and_hidden_files() {
        assert_eq!(
            search("rg --hidden -C 1 -g '*.rs' println", &files()),
            SearchResult::Output {
                output: "src/main.rs-1-fn main() {\nsrc/main.rs:2:    println!(\"Hello\");\nsrc/main.rs-3-}\n".to_owned()
            }
        );
        assert_eq!(
            search("rg --hidden hello .hidden", &files()),
            SearchResult::Output {
                output: ".hidden/config:1:hello\n".to_owned()
            }
        );
        assert_eq!(
            search("rg hello .hidden", &files()),
            SearchResult::Output {
                output: String::new()
            }
        );
    }

    #[test]
    fn lists_loaded_files_with_path_glob_and_hidden_filters() {
        assert_eq!(
            search("rg --files", &files()),
            SearchResult::Output {
                output: "src/main.rs\nREADME.md\n".to_owned()
            }
        );
        assert_eq!(
            search("rg --files -g '*.rs'", &files()),
            SearchResult::Output {
                output: "src/main.rs\n".to_owned()
            }
        );
        assert_eq!(
            search("rg --files --hidden .hidden", &files()),
            SearchResult::Output {
                output: ".hidden/config\n".to_owned()
            }
        );
    }

    #[test]
    fn reports_invalid_commands_with_help() {
        match search("rg --typo hello", &files()) {
            SearchResult::Error { error, help } => {
                assert!(error.contains("Unsupported option"));
                assert!(help.contains("Usage:"));
            }
            SearchResult::Output { .. } => panic!("expected an error"),
        }
        match search("grep hello", &files()) {
            SearchResult::Error { error, .. } => assert!(error.contains("Expected an rg command")),
            SearchResult::Output { .. } => panic!("expected an error"),
        }
    }

    #[test]
    fn supports_fixed_strings_and_matching_only() {
        assert_eq!(
            search("rg -F -o 'println!'", &files()),
            SearchResult::Output {
                output: "src/main.rs:2:println!\n".to_owned()
            }
        );
    }
}
