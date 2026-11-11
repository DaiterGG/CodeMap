use regex::{Regex, RegexBuilder};
use serde::Serialize;

const HELP: &str = include_str!("../../prompts/tools/sed/help.txt");

#[derive(Debug, PartialEq, Eq, Serialize)]
#[serde(untagged)]
pub(super) enum SedResult {
    Output { output: String },
    Error { error: String, help: &'static str },
}

#[derive(Clone)]
enum Address {
    Line(usize),
    Last,
    Pattern(Regex),
}

#[derive(Clone)]
enum Selection {
    All,
    Single(Address),
    Range(Address, Address),
}

#[derive(Clone)]
enum Operation {
    Delete,
    Print,
    Number,
    Quit,
    Substitute {
        regex: Regex,
        replacement: String,
        global: bool,
        print: bool,
    },
}

struct ScriptCommand {
    selection: Selection,
    operation: Operation,
    range_active: bool,
}

pub(super) fn run(command: &str, files: &[super::FileInput]) -> SedResult {
    match execute(command, files) {
        Ok(output) => SedResult::Output { output },
        Err(error) => SedResult::Error { error, help: HELP },
    }
}

fn execute(command: &str, files: &[super::FileInput]) -> Result<String, String> {
    let tokens = super::cat::tokenize(command)?;
    let Some(command_name) = tokens.first() else {
        return Err("Missing sed command.".to_owned());
    };
    if !matches!(command_name.as_str(), "sed" | "gsed") {
        return Err(format!("Expected a sed command, got '{command_name}'."));
    }

    let mut quiet = false;
    let mut extended = false;
    let mut expressions = Vec::new();
    let mut paths = Vec::new();
    let mut parse_options = true;
    let mut index = 1;
    while index < tokens.len() {
        let token = &tokens[index];
        if parse_options && token == "--" {
            parse_options = false;
        } else if parse_options && (token == "--help" || token == "-h") {
            return Ok(HELP.to_owned());
        } else if parse_options && token == "--expression" {
            index += 1;
            expressions.push(
                tokens
                    .get(index)
                    .ok_or_else(|| "--expression requires a script.".to_owned())?
                    .clone(),
            );
        } else if parse_options && token.starts_with("--expression=") {
            expressions.push(token["--expression=".len()..].to_owned());
        } else if parse_options && token.starts_with("--") {
            return Err(format!("Unsupported option: {token}"));
        } else if parse_options && token.starts_with('-') && token != "-" {
            for flag in token[1..].chars() {
                match flag {
                    'n' => quiet = true,
                    'E' | 'r' => extended = true,
                    'e' => {
                        index += 1;
                        expressions.push(
                            tokens
                                .get(index)
                                .ok_or_else(|| format!("-{flag} requires a script."))?
                                .clone(),
                        );
                    }
                    _ => return Err(format!("Unsupported option: -{flag}")),
                }
            }
        } else if expressions.is_empty() {
            expressions.push(token.clone());
        } else {
            paths.push(token.clone());
        }
        index += 1;
    }
    if expressions.is_empty() {
        return Err("Missing sed script.".to_owned());
    }
    if paths.is_empty() {
        return Err("Specify at least one loaded file path.".to_owned());
    }
    let script = expressions.join(";");
    let mut program = parse_script(&script, extended)?;
    if files.is_empty() {
        return Err("No source files are currently loaded.".to_owned());
    }
    let selected = paths
        .iter()
        .map(|path| {
            if path == "-" {
                return Err(
                    "Reading standard input is not supported; specify a loaded file path."
                        .to_owned(),
                );
            }
            super::cat::validate_repo_path(path)?;
            files
                .iter()
                .find(|file| super::cat::paths_equal(&file.path, path))
                .ok_or_else(|| format!("File is not loaded in this repository: {path}"))
        })
        .collect::<Result<Vec<_>, _>>()?;

    let mut output = String::new();
    let mut global_line_number = 0usize;
    let total_lines = count_input_lines(&selected);
    let mut quit = false;
    for file in &selected {
        for line in file.contents.split_inclusive('\n') {
            global_line_number += 1;
            let has_newline = line.ends_with('\n');
            let mut current = line.strip_suffix('\n').unwrap_or(line).to_owned();
            let mut deleted = false;

            for command in &mut program {
                if !command_applies(
                    command,
                    &current,
                    global_line_number,
                    global_line_number == total_lines,
                ) {
                    continue;
                }
                match &command.operation {
                    Operation::Delete => {
                        deleted = true;
                        break;
                    }
                    Operation::Print => {
                        append_line(&mut output, &current, has_newline);
                    }
                    Operation::Number => {
                        output.push_str(&format!("{global_line_number}\n"));
                    }
                    Operation::Quit => {
                        quit = true;
                        break;
                    }
                    Operation::Substitute {
                        regex,
                        replacement,
                        global,
                        print,
                    } => {
                        let replaced_match = regex.is_match(&current);
                        let replaced = if *global {
                            regex.replace_all(&current, replacement.as_str())
                        } else {
                            regex.replace(&current, replacement.as_str())
                        };
                        if replaced_match {
                            current = replaced.into_owned();
                            if *print {
                                append_line(&mut output, &current, has_newline);
                            }
                        }
                    }
                }
                if quit {
                    break;
                }
            }

            if !deleted && !quiet {
                append_line(&mut output, &current, has_newline);
            }
            if quit {
                return Ok(output);
            }
        }
    }
    Ok(output)
}

fn count_input_lines(files: &[&super::FileInput]) -> usize {
    files
        .iter()
        .map(|file| file.contents.split_inclusive('\n').count())
        .sum()
}

fn append_line(output: &mut String, line: &str, has_newline: bool) {
    output.push_str(line);
    if has_newline {
        output.push('\n');
    }
}

fn command_applies(
    command: &mut ScriptCommand,
    line: &str,
    line_number: usize,
    is_last_line: bool,
) -> bool {
    match &command.selection {
        Selection::All => true,
        Selection::Single(address) => address_matches(address, line, line_number, is_last_line),
        Selection::Range(start, end) => {
            if command.range_active {
                if address_matches(end, line, line_number, is_last_line) {
                    command.range_active = false;
                }
                true
            } else if address_matches(start, line, line_number, is_last_line) {
                if !address_matches(end, line, line_number, is_last_line) {
                    command.range_active = true;
                }
                true
            } else {
                false
            }
        }
    }
}

fn address_matches(address: &Address, line: &str, line_number: usize, is_last_line: bool) -> bool {
    match address {
        Address::Line(number) => line_number == *number,
        Address::Last => is_last_line,
        Address::Pattern(regex) => regex.is_match(line),
    }
}

fn parse_script(script: &str, extended: bool) -> Result<Vec<ScriptCommand>, String> {
    let mut parser = ScriptParser {
        chars: script.chars().collect(),
        index: 0,
        extended,
    };
    let mut commands = Vec::new();
    while parser.skip_separators() {
        let selection = parser.parse_selection()?;
        let operation = parser.parse_operation()?;
        commands.push(ScriptCommand {
            selection,
            operation,
            range_active: false,
        });
    }
    if commands.is_empty() {
        return Err("Sed script is empty.".to_owned());
    }
    Ok(commands)
}

struct ScriptParser {
    chars: Vec<char>,
    index: usize,
    extended: bool,
}

impl ScriptParser {
    fn skip_separators(&mut self) -> bool {
        while self
            .chars
            .get(self.index)
            .is_some_and(|character| character.is_whitespace() || *character == ';')
        {
            self.index += 1;
        }
        self.index < self.chars.len()
    }

    fn parse_selection(&mut self) -> Result<Selection, String> {
        self.skip_spaces();
        let Some(start) = self.parse_address()? else {
            return Ok(Selection::All);
        };
        self.skip_spaces();
        if self.consume(',') {
            let end = self
                .parse_address()?
                .ok_or_else(|| "Missing end address in sed range.".to_owned())?;
            Ok(Selection::Range(start, end))
        } else {
            Ok(Selection::Single(start))
        }
    }

    fn parse_address(&mut self) -> Result<Option<Address>, String> {
        let Some(character) = self.peek() else {
            return Ok(None);
        };
        if character.is_ascii_digit() {
            let start = self.index;
            while self.peek().is_some_and(|digit| digit.is_ascii_digit()) {
                self.index += 1;
            }
            let number = self.chars[start..self.index]
                .iter()
                .collect::<String>()
                .parse()
                .map_err(|_| "Invalid numeric address.".to_owned())?;
            if number == 0 {
                return Err("Sed line addresses start at 1.".to_owned());
            }
            return Ok(Some(Address::Line(number)));
        }
        if character == '$' {
            self.index += 1;
            return Ok(Some(Address::Last));
        }
        if character == '/' {
            self.index += 1;
            let pattern = self.read_delimited('/')?;
            return Ok(Some(Address::Pattern(build_regex(
                &pattern,
                self.extended,
            )?)));
        }
        Ok(None)
    }

    fn parse_operation(&mut self) -> Result<Operation, String> {
        self.skip_spaces();
        let operation = match self.next() {
            Some('d') => Operation::Delete,
            Some('p') => Operation::Print,
            Some('=') => Operation::Number,
            Some('q') => Operation::Quit,
            Some('s') => self.parse_substitution()?,
            Some(other) => return Err(format!("Unsupported sed command: {other}")),
            None => return Err("Missing command after sed address.".to_owned()),
        };
        self.skip_spaces();
        if self.index < self.chars.len() && self.peek() != Some(';') {
            return Err(format!(
                "Unexpected text in sed script near '{}'.",
                self.chars[self.index..].iter().collect::<String>()
            ));
        }
        Ok(operation)
    }

    fn parse_substitution(&mut self) -> Result<Operation, String> {
        let delimiter = self
            .next()
            .ok_or_else(|| "Missing delimiter for substitution command.".to_owned())?;
        if delimiter.is_ascii_alphanumeric() || delimiter.is_whitespace() || delimiter == '\\' {
            return Err("Substitution delimiter must be punctuation.".to_owned());
        }
        let pattern = self.read_delimited(delimiter)?;
        let replacement_text = self.read_delimited(delimiter)?;
        let mut flags = String::new();
        while self
            .peek()
            .is_some_and(|character| character.is_ascii_alphabetic())
        {
            flags.push(self.next().expect("peeked flag exists"));
        }
        let mut global = false;
        let mut print = false;
        for flag in flags.chars() {
            match flag {
                'g' => global = true,
                'p' => print = true,
                _ => return Err(format!("Unsupported substitution flag: {flag}")),
            }
        }
        let replacement = translate_replacement(&replacement_text);
        Ok(Operation::Substitute {
            regex: build_regex(&pattern, self.extended)?,
            replacement,
            global,
            print,
        })
    }

    fn read_delimited(&mut self, delimiter: char) -> Result<String, String> {
        let mut value = String::new();
        let mut escaped = false;
        while let Some(character) = self.next() {
            if escaped {
                if character == delimiter {
                    value.push(delimiter);
                } else {
                    value.push('\\');
                    value.push(character);
                }
                escaped = false;
            } else if character == '\\' {
                escaped = true;
            } else if character == delimiter {
                return Ok(value);
            } else {
                value.push(character);
            }
        }
        if escaped {
            value.push('\\');
        }
        Err("Unterminated pattern or replacement in sed script.".to_owned())
    }

    fn skip_spaces(&mut self) {
        while self.peek().is_some_and(char::is_whitespace) {
            self.index += 1;
        }
    }

    fn peek(&self) -> Option<char> {
        self.chars.get(self.index).copied()
    }

    fn next(&mut self) -> Option<char> {
        let character = self.peek()?;
        self.index += 1;
        Some(character)
    }

    fn consume(&mut self, expected: char) -> bool {
        if self.peek() == Some(expected) {
            self.index += 1;
            true
        } else {
            false
        }
    }
}

fn build_regex(pattern: &str, extended: bool) -> Result<Regex, String> {
    let pattern = if extended {
        pattern.to_owned()
    } else {
        translate_basic_regex(pattern)
    };
    RegexBuilder::new(&pattern)
        .build()
        .map_err(|error| format!("Invalid sed regular expression: {error}"))
}

fn translate_basic_regex(pattern: &str) -> String {
    let mut translated = String::new();
    let mut escaped = false;
    for character in pattern.chars() {
        if escaped {
            if matches!(character, '(' | ')' | '{' | '}' | '+' | '?' | '|') {
                translated.push(character);
            } else {
                translated.push('\\');
                translated.push(character);
            }
            escaped = false;
        } else if character == '\\' {
            escaped = true;
        } else {
            if matches!(character, '(' | ')' | '{' | '}' | '+' | '?' | '|') {
                translated.push('\\');
            }
            translated.push(character);
        }
    }
    if escaped {
        translated.push('\\');
    }
    translated
}

fn translate_replacement(replacement: &str) -> String {
    let mut translated = String::new();
    let mut escaped = false;
    for character in replacement.chars() {
        if escaped {
            if character.is_ascii_digit() {
                translated.push('$');
                translated.push(character);
            } else {
                if character == '$' {
                    translated.push_str("$$");
                } else {
                    translated.push(character);
                }
            }
            escaped = false;
        } else if character == '\\' {
            escaped = true;
        } else if character == '&' {
            translated.push_str("$0");
        } else if character == '$' {
            translated.push_str("$$");
        } else {
            translated.push(character);
        }
    }
    if escaped {
        translated.push('\\');
    }
    translated
}

#[cfg(test)]
mod tests {
    use super::*;

    fn files() -> Vec<super::super::FileInput> {
        vec![
            super::super::FileInput {
                path: "src/main.rs".to_owned(),
                contents: "alpha beta\nbeta gamma\nlast\n".to_owned(),
            },
            super::super::FileInput {
                path: "README.md".to_owned(),
                contents: "alpha docs\n".to_owned(),
            },
        ]
    }

    #[test]
    fn substitutes_on_selected_file() {
        assert_eq!(
            run("sed 's/beta/B/g' src/main.rs", &files()),
            SedResult::Output {
                output: "alpha B\nB gamma\nlast\n".to_owned()
            }
        );
    }

    #[test]
    fn supports_quiet_mode_addresses_and_multiple_files() {
        assert_eq!(
            run("sed -n -e '1p' -e '$p' src/main.rs README.md", &files()),
            SedResult::Output {
                output: "alpha beta\nalpha docs\n".to_owned()
            }
        );
        assert_eq!(
            run("sed '1,2d' src/main.rs", &files()),
            SedResult::Output {
                output: "last\n".to_owned()
            }
        );
    }

    #[test]
    fn supports_back_references_and_bre_groups() {
        assert_eq!(
            run("sed 's/\\(alpha\\) beta/\\1!/' src/main.rs", &files()),
            SedResult::Output {
                output: "alpha!\nbeta gamma\nlast\n".to_owned()
            }
        );
    }

    #[test]
    fn supports_regex_addresses_and_substitution_print_flag() {
        assert_eq!(
            run("sed -n '/beta/p' src/main.rs", &files()),
            SedResult::Output {
                output: "alpha beta\nbeta gamma\n".to_owned()
            }
        );
        assert_eq!(
            run("sed -n 's/beta/B/p' src/main.rs", &files()),
            SedResult::Output {
                output: "alpha B\nB gamma\n".to_owned()
            }
        );
    }

    #[test]
    fn prints_unchanged_lines_automatically_and_keeps_dollars_literal() {
        assert_eq!(
            run("sed 's/last/$HOME/' src/main.rs", &files()),
            SedResult::Output {
                output: "alpha beta\nbeta gamma\n$HOME\n".to_owned()
            }
        );
    }

    #[test]
    fn returns_help_for_unsupported_commands_and_paths() {
        for command in [
            "sed -i 's/a/b/' src/main.rs",
            "sed 'x' src/main.rs",
            "sed 'p' ../secret",
        ] {
            match run(command, &files()) {
                SedResult::Error { error, help } => {
                    assert!(!error.is_empty());
                    assert!(help.contains("Usage: sed"));
                }
                SedResult::Output { .. } => panic!("expected an error"),
            }
        }
    }
}
