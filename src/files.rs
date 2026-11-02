//! Recursively collects text files from a directory into the source context sent
//! to the LLM, honoring Git ignore rules and optional README/Markdown exclusions.

use base64::Engine;
use base64::engine::general_purpose::STANDARD;
use std::collections::HashSet;
use std::fs;
use std::io::{self, Read, Write};
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::sync::atomic::{AtomicU64, Ordering};

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct ReadOptions {
    pub exclude_readme: bool,
    pub exclude_markdown: bool,
    pub include_non_text: bool,
    pub exclude_gitignored: bool,
}

impl Default for ReadOptions {
    fn default() -> Self {
        Self {
            exclude_readme: false,
            exclude_markdown: false,
            include_non_text: false,
            exclude_gitignored: true,
        }
    }
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
    let mut paths = Vec::new();
    append_paths(root, &mut paths)?;
    let ignored = if options.exclude_gitignored {
        git_ignored_paths(root, &paths)?
    } else {
        HashSet::new()
    };

    let mut files = Vec::new();
    for path in paths {
        let relative_path = path.strip_prefix(root).unwrap_or(&path);
        if should_exclude(&path, options) || ignored.contains(&git_path(relative_path)) {
            continue;
        }
        if let Some(file) = read_file(root, &path, options)? {
            files.push(file);
        }
    }
    Ok(files)
}

fn append_paths(directory: &Path, paths: &mut Vec<PathBuf>) -> io::Result<()> {
    let entries = fs::read_dir(directory).map_err(|error| {
        io::Error::new(
            error.kind(),
            format!("could not read directory {}: {error}", directory.display()),
        )
    })?;
    let mut entries = entries
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
    entries.sort();

    for path in entries {
        if path.file_name().is_some_and(|name| name == ".git") {
            continue;
        }
        let file_type = fs::symlink_metadata(&path)
            .map_err(|error| {
                io::Error::new(
                    error.kind(),
                    format!("could not inspect {}: {error}", path.display()),
                )
            })?
            .file_type();

        if file_type.is_dir() {
            append_paths(&path, paths)?;
        } else if file_type.is_file() {
            paths.push(path);
        }
    }

    Ok(())
}

fn git_path(path: &Path) -> String {
    path.to_string_lossy().replace('\\', "/")
}

fn git_ignored_paths(root: &Path, paths: &[PathBuf]) -> io::Result<HashSet<String>> {
    if paths.is_empty() {
        return Ok(HashSet::new());
    }

    let mut input = Vec::new();
    for path in paths {
        input.extend_from_slice(git_path(path.strip_prefix(root).unwrap_or(path)).as_bytes());
        input.push(0);
    }

    let in_git_worktree = is_git_worktree(root)?;
    let temporary_git_dir = if !in_git_worktree {
        Some(TemporaryGitDir::new()?)
    } else {
        None
    };
    let mut command = Command::new("git");
    if in_git_worktree {
        command.arg("-C").arg(root);
    } else if let Some(temporary_git_dir) = &temporary_git_dir {
        command
            .arg(format!("--git-dir={}", temporary_git_dir.0.display()))
            .arg(format!("--work-tree={}", root.display()));
    }
    let mut child = command
        .args(["check-ignore", "--no-index", "--stdin", "-z"])
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .map_err(|error| io::Error::other(format!("could not run git check-ignore: {error}")))?;
    let mut stdout = child.stdout.take().expect("piped stdout is available");
    let stdout_reader = std::thread::spawn(move || {
        let mut output = Vec::new();
        stdout.read_to_end(&mut output).map(|_| output)
    });
    let mut stderr = child.stderr.take().expect("piped stderr is available");
    let stderr_reader = std::thread::spawn(move || {
        let mut output = Vec::new();
        stderr.read_to_end(&mut output).map(|_| output)
    });
    let mut stdin = child.stdin.take().expect("piped stdin is available");
    let write_result = stdin.write_all(&input);
    drop(stdin);
    let status = child.wait()?;
    let stdout = stdout_reader
        .join()
        .map_err(|_| io::Error::other("git check-ignore output reader panicked"))??;
    let stderr = stderr_reader
        .join()
        .map_err(|_| io::Error::other("git check-ignore error reader panicked"))??;
    write_result?;

    if !status.success() && status.code() != Some(1) {
        return Err(io::Error::other(format!(
            "git check-ignore failed: {}",
            String::from_utf8_lossy(&stderr).trim()
        )));
    }
    Ok(stdout
        .split(|byte| *byte == 0)
        .filter(|path| !path.is_empty())
        .map(|path| String::from_utf8_lossy(path).replace('\\', "/"))
        .collect())
}

fn is_git_worktree(root: &Path) -> io::Result<bool> {
    let output = Command::new("git")
        .arg("-C")
        .arg(root)
        .args(["rev-parse", "--absolute-git-dir"])
        .output()
        .map_err(|error| io::Error::other(format!("could not run git: {error}")))?;
    if !output.status.success() {
        return Ok(false);
    }

    let path = String::from_utf8_lossy(&output.stdout).trim().to_owned();
    Ok(!path.is_empty())
}

struct TemporaryGitDir(PathBuf);

impl TemporaryGitDir {
    fn new() -> io::Result<Self> {
        static NEXT_ID: AtomicU64 = AtomicU64::new(0);

        let parent = std::env::temp_dir();
        let path = loop {
            let candidate = parent.join(format!(
                "hello-cli-gitignore-{}-{}",
                std::process::id(),
                NEXT_ID.fetch_add(1, Ordering::Relaxed)
            ));
            match fs::create_dir(&candidate) {
                Ok(()) => break candidate,
                Err(error) if error.kind() == io::ErrorKind::AlreadyExists => continue,
                Err(error) => return Err(error),
            }
        };
        let git_dir = Self(path);
        let output = Command::new("git")
            .args(["init", "--bare", "--quiet"])
            .arg(&git_dir.0)
            .output()
            .map_err(|error| {
                io::Error::other(format!("could not initialize Git ignore rules: {error}"))
            })?;
        if !output.status.success() {
            return Err(io::Error::other(format!(
                "could not initialize Git ignore rules: {}",
                String::from_utf8_lossy(&output.stderr).trim()
            )));
        }
        Ok(git_dir)
    }
}

impl Drop for TemporaryGitDir {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.0);
    }
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
                ..ReadOptions::default()
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
    fn excludes_git_ignored_files_by_default_and_allows_negated_rules() {
        let unique = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .as_nanos();
        let root = std::env::temp_dir().join(format!("hello-cli-gitignore-{unique}"));
        let nested = root.join("nested");
        fs::create_dir_all(&nested).unwrap();
        fs::write(root.join(".gitignore"), "*.log\n").unwrap();
        fs::write(nested.join(".gitignore"), "*.tmp\n!important.tmp\n").unwrap();
        fs::write(root.join("ignored.log"), "ignored").unwrap();
        fs::write(root.join("kept.rs"), "kept").unwrap();
        fs::write(nested.join("ignored.tmp"), "ignored").unwrap();
        fs::write(nested.join("important.tmp"), "important").unwrap();

        let files = read_directory_files(&root).unwrap();
        let paths = files
            .iter()
            .map(|file| git_path(&file.relative_path()))
            .collect::<Vec<_>>();
        assert_eq!(
            paths,
            vec![
                ".gitignore",
                "kept.rs",
                "nested/.gitignore",
                "nested/important.tmp"
            ]
        );

        fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn can_include_git_ignored_files_when_filtering_is_disabled() {
        let unique = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .as_nanos();
        let root = std::env::temp_dir().join(format!("hello-cli-gitignore-optout-{unique}"));
        fs::create_dir_all(&root).unwrap();
        fs::write(root.join(".gitignore"), "ignored.log\n").unwrap();
        fs::write(root.join("ignored.log"), "included").unwrap();

        let files = read_directory_files_with_options(
            &root,
            ReadOptions {
                exclude_gitignored: false,
                ..ReadOptions::default()
            },
        )
        .unwrap();
        assert!(files.iter().any(|file| file.name == "ignored"));

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
