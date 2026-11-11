use serde::Serialize;
use std::path::{Component, Path};

const HELP: &str = include_str!("../../prompts/tools/cat/help.txt");

#[derive(Debug, PartialEq, Eq, Serialize)]
#[serde(untagged)]
pub(super) enum CatResult {
    Output { output: String },
    Error { error: String, help: &'static str },
}

#[derive(Default)]
struct Options {
    number: bool,
    number_nonblank: bool,
    squeeze_blank: bool,
    show_ends: bool,
    show_tabs: bool,
    show_nonprinting: bool,
    paths: Vec<String>,
}

pub(super) fn run(command: &str, files: &[super::FileInput]) -> CatResult {
    match execute(command, files) {
        Ok(output) => CatResult::Output { output },
        Err(error) => CatResult::Error { error, help: HELP },
    }
}

fn execute(command: &str, files: &[super::FileInput]) -> Result<String, String> {
    let tokens = tokenize(command)?;
    let Some(command_name) = tokens.first() else {
        return Err("Missing cat command.".to_owned());
    };
    if !matches!(command_name.as_str(), "cat" | "concatenate") {
        return Err(format!("Expected a cat command, got '{command_name}'."));
    }

    let mut options = Options::default();
    let mut parse_options = true;
    for token in &tokens[1..] {
        if parse_options && token == "--" {
            parse_options = false;
        } else if parse_options && (token == "--help" || token == "-h") {
            return Ok(HELP.to_owned());
        } else if parse_options && token.starts_with("--") {
            match token.as_str() {
                "--number" => options.number = true,
                "--number-nonblank" => options.number_nonblank = true,
                "--squeeze-blank" => options.squeeze_blank = true,
                "--show-ends" => options.show_ends = true,
                "--show-tabs" => options.show_tabs = true,
                "--show-nonprinting" => options.show_nonprinting = true,
                _ => return Err(format!("Unsupported option: {token}")),
            }
        } else if parse_options && token.starts_with('-') && token != "-" {
            for flag in token[1..].chars() {
                match flag {
                    'n' => options.number = true,
                    'b' => options.number_nonblank = true,
                    's' => options.squeeze_blank = true,
                    'E' => options.show_ends = true,
                    'T' => options.show_tabs = true,
                    'v' => options.show_nonprinting = true,
                    'e' => {
                        options.show_nonprinting = true;
                        options.show_ends = true;
                    }
                    't' => {
                        options.show_nonprinting = true;
                        options.show_tabs = true;
                    }
                    'A' => {
                        options.show_nonprinting = true;
                        options.show_ends = true;
                        options.show_tabs = true;
                    }
                    _ => return Err(format!("Unsupported option: -{flag}")),
                }
            }
        } else {
            options.paths.push(token.clone());
        }
    }

    if options.paths.is_empty() {
        return Err("Specify at least one loaded file path.".to_owned());
    }

    let mut output = String::new();
    let mut line_number = 1usize;
    let mut previous_line_blank = false;
    for path in &options.paths {
        if path == "-" {
            return Err(
                "Reading standard input is not supported; specify a loaded file path.".to_owned(),
            );
        }
        validate_repo_path(path)?;
        let file = files
            .iter()
            .find(|file| paths_equal(&file.path, path))
            .ok_or_else(|| format!("File is not loaded in this repository: {path}"))?;

        for line in file.contents.split_inclusive('\n') {
            let has_newline = line.ends_with('\n');
            let text = line.strip_suffix('\n').unwrap_or(line);
            let is_blank = text.is_empty();
            if options.squeeze_blank && is_blank && previous_line_blank {
                continue;
            }
            previous_line_blank = is_blank;

            if options.number_nonblank && !is_blank {
                output.push_str(&format!("{line_number:>6}\t"));
                line_number += 1;
            } else if options.number && !options.number_nonblank {
                output.push_str(&format!("{line_number:>6}\t"));
                line_number += 1;
            }

            append_visible_text(&mut output, text, &options);
            if has_newline {
                if options.show_ends {
                    output.push('$');
                }
                output.push('\n');
            } else if options.show_ends {
                output.push('$');
            }
        }
    }
    Ok(output)
}

fn append_visible_text(output: &mut String, text: &str, options: &Options) {
    for character in text.chars() {
        if character == '\t' && options.show_tabs {
            output.push_str("^I");
        } else if options.show_nonprinting && character.is_control() {
            if character == '\u{7f}' {
                output.push_str("^?");
            } else if (character as u32) < 0x20 {
                output.push('^');
                output.push(char::from_u32(character as u32 + 0x40).unwrap_or(character));
            } else {
                output.push(character);
            }
        } else {
            output.push(character);
        }
    }
}

pub(super) fn validate_repo_path(path: &str) -> Result<(), String> {
    let normalized = path.replace('\\', "/");
    let path_buf = Path::new(&normalized);
    if path_buf.is_absolute()
        || normalized
            .split('/')
            .next()
            .is_some_and(|component| component.ends_with(':'))
        || path_buf
            .components()
            .any(|component| !matches!(component, Component::Normal(_)))
    {
        return Err(format!(
            "Path must stay within the loaded repository: {normalized}"
        ));
    }
    Ok(())
}

pub(super) fn paths_equal(loaded_path: &str, requested_path: &str) -> bool {
    loaded_path.replace('\\', "/") == requested_path.replace('\\', "/")
}

pub(super) fn tokenize(command: &str) -> Result<Vec<String>, String> {
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
                contents: "fn main() {\n\tprintln!(\"hello\");\n}\n".to_owned(),
            },
            super::super::FileInput {
                path: "README with spaces.md".to_owned(),
                contents: "first\n\n\nthird".to_owned(),
            },
        ]
    }

    #[test]
    fn concatenates_files_in_the_given_order() {
        assert_eq!(
            run("cat 'README with spaces.md' src/main.rs", &files()),
            CatResult::Output {
                output: "first\n\n\nthirdfn main() {\n\tprintln!(\"hello\");\n}\n".to_owned()
            }
        );
    }

    #[test]
    fn supports_line_numbers_tabs_and_squeezing_blank_lines() {
        assert_eq!(
            run("cat -b -s -T 'README with spaces.md' src/main.rs", &files()),
            CatResult::Output {
                output: "     1\tfirst\n\n     2\tthird     3\tfn main() {\n     4\t^Iprintln!(\"hello\");\n     5\t}\n".to_owned()
            }
        );
    }

    #[test]
    fn reports_invalid_commands_and_unloaded_paths_with_help() {
        for command in [
            "cat --unknown src/main.rs",
            "cat ../secret",
            "cat missing.rs",
        ] {
            match run(command, &files()) {
                CatResult::Error { error, help } => {
                    assert!(!error.is_empty());
                    assert!(help.contains("Usage: cat"));
                }
                CatResult::Output { .. } => panic!("expected an error"),
            }
        }
    }
}
