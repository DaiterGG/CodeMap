//! Recursively collects text files from a directory into the source context sent
//! to the LLM, with optional exclusions for README and Markdown files.

use base64::Engine;
use base64::engine::general_purpose::STANDARD;
use std::fs;
use std::io;
use std::path::{Path, PathBuf};

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct ReadOptions {
    pub exclude_readme: bool,
    pub exclude_markdown: bool,
    pub include_non_text: bool,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct SourceFile {
    pub path: PathBuf,
    pub name: String,
    pub extension: Option<String>,
    pub contents: String,
}

impl SourceFile {
    fn relative_path(&self) -> PathBuf {
        let mut filename = self.name.clone();
        if let Some(extension) = &self.extension {
            filename.push('.');
            filename.push_str(extension);
        }
        self.path.join(filename)
    }

    fn prompt_text(&self) -> String {
        let relative_path = self
            .relative_path()
            .display()
            .to_string()
            .replace('\\', "/");
        let mut text = format!("``` {relative_path}\n");
        for (line_number, line) in self.contents.lines().enumerate() {
            text.push_str(&(line_number + 1).to_string());
            text.push(' ');
            text.push_str(line);
            text.push('\n');
        }
        text.push_str("```\n");
        text
    }
}

pub fn format_source_files(files: &[SourceFile]) -> String {
    let mut output = String::new();
    for file in files {
        output.push_str(&file.prompt_text());
        output.push('\n');
    }
    output
}

pub fn read_directory(path: impl AsRef<Path>) -> io::Result<String> {
    read_directory_files(path).map(|files| format_source_files(&files))
}

pub fn read_directory_files(path: impl AsRef<Path>) -> io::Result<Vec<SourceFile>> {
    read_directory_files_with_options(path, ReadOptions::default())
}

pub fn read_directory_with_options(
    path: impl AsRef<Path>,
    options: ReadOptions,
) -> io::Result<String> {
    read_directory_files_with_options(path, options).map(|files| format_source_files(&files))
}

pub fn read_directory_files_with_options(
    path: impl AsRef<Path>,
    options: ReadOptions,
) -> io::Result<Vec<SourceFile>> {
    let root = path.as_ref();
    let mut files = Vec::new();
    append_directory(root, root, options, &mut files)?;
    Ok(files)
}

fn append_directory(
    root: &Path,
    directory: &Path,
    options: ReadOptions,
    files: &mut Vec<SourceFile>,
) -> io::Result<()> {
    let entries = fs::read_dir(directory).map_err(|error| {
        io::Error::new(
            error.kind(),
            format!("could not read directory {}: {error}", directory.display()),
        )
    })?;
    let mut paths = entries
        .map(|entry| {
            entry.map(|entry| entry.path()).map_err(|error| {
                io::Error::new(
                    error.kind(),
                    format!(
                        "could not read an entry in {}: {error}",
                        directory.display()
                    ),
                )
            })
        })
        .collect::<io::Result<Vec<_>>>()?;
    paths.sort();

    for path in paths {
        let file_type = fs::symlink_metadata(&path)
            .map_err(|error| {
                io::Error::new(
                    error.kind(),
                    format!("could not inspect {}: {error}", path.display()),
                )
            })?
            .file_type();

        if file_type.is_dir() {
            append_directory(root, &path, options, files)?;
        } else if file_type.is_file() {
            if !should_exclude(&path, options) {
                if let Some(file) = read_file(root, &path, options)? {
                    files.push(file);
                }
            }
        }
    }

    Ok(())
}

fn should_exclude(path: &Path, options: ReadOptions) -> bool {
    if options.exclude_readme
        && path
            .file_stem()
            .is_some_and(|stem| stem.eq_ignore_ascii_case("readme"))
    {
        return true;
    }
    options.exclude_markdown
        && path.extension().is_some_and(|extension| {
            extension.eq_ignore_ascii_case("md") || extension.eq_ignore_ascii_case("markdown")
        })
}

fn read_file(root: &Path, path: &Path, options: ReadOptions) -> io::Result<Option<SourceFile>> {
    let relative_path = path.strip_prefix(root).unwrap_or(path);
    let bytes = fs::read(path).map_err(|error| {
        io::Error::new(
            error.kind(),
            format!("could not read {}: {error}", path.display()),
        )
    })?;
    let contents = match std::str::from_utf8(&bytes) {
        Ok(contents)
            if !contents.chars().any(|character| {
                character.is_control() && !matches!(character, '\t' | '\n' | '\r')
            }) =>
        {
            contents.to_owned()
        }
        _ if options.include_non_text => {
            format!("[binary file, base64 encoded]\n{}", STANDARD.encode(bytes))
        }
        _ => return Ok(None),
    };

    let name = relative_path
        .file_stem()
        .unwrap_or_else(|| relative_path.file_name().unwrap_or_default())
        .to_string_lossy()
        .into_owned();
    let extension = relative_path
        .extension()
        .map(|extension| extension.to_string_lossy().into_owned());
    let parent = relative_path.parent().unwrap_or_else(|| Path::new(""));

    Ok(Some(SourceFile {
        path: parent.to_path_buf(),
        name,
        extension,
        contents,
    }))
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::time::{SystemTime, UNIX_EPOCH};

    #[test]
    fn reads_nested_text_files_in_sorted_order_with_paths() {
        let unique = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .as_nanos();
        let root = std::env::temp_dir().join(format!("hello-cli-files-{unique}"));
        let nested = root.join("nested");
        fs::create_dir_all(&nested).unwrap();
        fs::write(root.join("z.txt"), "last").unwrap();
        fs::write(nested.join("a.txt"), "first\nsecond\n").unwrap();

        let result = read_directory_files(&root).unwrap();
        assert_eq!(
            result,
            vec![
                SourceFile {
                    path: PathBuf::from("nested"),
                    name: "a".to_owned(),
                    extension: Some("txt".to_owned()),
                    contents: "first\nsecond\n".to_owned(),
                },
                SourceFile {
                    path: PathBuf::new(),
                    name: "z".to_owned(),
                    extension: Some("txt".to_owned()),
                    contents: "last".to_owned(),
                },
            ]
        );
        assert_eq!(
            format_source_files(&result),
            "``` nested/a.txt\n1 first\n2 second\n```\n\n``` z.txt\n1 last\n```\n\n"
        );
        assert_eq!(read_directory(&root).unwrap(), format_source_files(&result));

        fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn excludes_readme_and_markdown_files_when_requested() {
        let unique = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .as_nanos();
        let root = std::env::temp_dir().join(format!("hello-cli-filter-{unique}"));
        fs::create_dir_all(root.join("nested")).unwrap();
        fs::write(root.join("README.md"), "readme markdown").unwrap();
        fs::write(root.join("readme.txt"), "readme text").unwrap();
        fs::write(root.join("guide.MARKDOWN"), "other markdown").unwrap();
        fs::write(root.join("notes.txt"), "keep").unwrap();
        fs::write(root.join("nested/README"), "nested readme").unwrap();

        let result = read_directory_files_with_options(
            &root,
            ReadOptions {
                exclude_readme: true,
                exclude_markdown: true,
                include_non_text: false,
            },
        )
        .unwrap();

        let prompt = format_source_files(&result);
        assert!(!prompt.contains("readme markdown"));
        assert!(!prompt.contains("readme text"));
        assert!(!prompt.contains("other markdown"));
        assert!(!prompt.contains("nested readme"));
        assert!(prompt.contains("keep"));

        fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn includes_binary_files_as_base64_when_requested() {
        let unique = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .as_nanos();
        let root = std::env::temp_dir().join(format!("hello-cli-binary-included-{unique}"));
        fs::create_dir_all(&root).unwrap();
        fs::write(root.join("image.dat"), [0x89, 0x50, 0x4e, 0x47, 0xff]).unwrap();
        fs::write(root.join("data.bin"), b"header\0body").unwrap();
        fs::write(root.join("source.rs"), "fn main() {}\n").unwrap();

        let files = read_directory_files_with_options(
            &root,
            ReadOptions {
                include_non_text: true,
                ..ReadOptions::default()
            },
        )
        .unwrap();

        assert_eq!(files.len(), 3);
        assert!(
            files[0]
                .contents
                .starts_with("[binary file, base64 encoded]\n")
        );
        assert!(
            files[1]
                .contents
                .starts_with("[binary file, base64 encoded]\n")
        );
        assert_eq!(files[2].contents, "fn main() {}\n");

        fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn skips_non_utf8_and_binary_files() {
        let unique = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .as_nanos();
        let root = std::env::temp_dir().join(format!("hello-cli-binary-{unique}"));
        fs::create_dir_all(&root).unwrap();
        fs::write(root.join("image.dat"), [0x89, 0x50, 0x4e, 0x47, 0xff]).unwrap();
        fs::write(root.join("data.bin"), b"header\0body").unwrap();
        fs::write(root.join("source.rs"), "fn main() {}\n").unwrap();

        let files = read_directory_files(&root).unwrap();
        assert_eq!(files.len(), 1);
        assert_eq!(files[0].name, "source");
        assert_eq!(files[0].extension.as_deref(), Some("rs"));
        assert_eq!(files[0].contents, "fn main() {}\n");

        fs::remove_dir_all(root).unwrap();
    }
}
