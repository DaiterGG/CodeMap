//! Recursively collects text files from a directory into the source context sent
//! to the LLM, with optional exclusions for README and Markdown files.

use std::fs;
use std::io;
use std::path::Path;

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct ReadOptions {
    pub exclude_readme: bool,
    pub exclude_markdown: bool,
}

pub fn read_directory(path: impl AsRef<Path>) -> io::Result<String> {
    read_directory_with_options(path, ReadOptions::default())
}

pub fn read_directory_with_options(
    path: impl AsRef<Path>,
    options: ReadOptions,
) -> io::Result<String> {
    let root = path.as_ref();
    let mut output = String::new();
    append_directory(root, root, options, &mut output)?;
    Ok(output)
}

fn append_directory(
    root: &Path,
    directory: &Path,
    options: ReadOptions,
    output: &mut String,
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
            append_directory(root, &path, options, output)?;
        } else if file_type.is_file() {
            if !should_exclude(&path, options) {
                append_file(root, &path, output)?;
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

fn append_file(root: &Path, path: &Path, output: &mut String) -> io::Result<()> {
    let relative_path = path.strip_prefix(root).unwrap_or(path);
    let contents = fs::read_to_string(path).map_err(|error| {
        io::Error::new(
            error.kind(),
            format!("could not read {}: {error}", path.display()),
        )
    })?;

    output.push_str("===== ");
    output.push_str(&relative_path.display().to_string());
    output.push_str(" =====\n");
    output.push_str(&contents);
    if !contents.ends_with('\n') {
        output.push('\n');
    }
    output.push('\n');

    Ok(())
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
        fs::write(nested.join("a.txt"), "first\n").unwrap();

        let result = read_directory(&root).unwrap();
        let nested_path = Path::new("nested").join("a.txt");
        assert_eq!(
            result,
            format!(
                "===== {} =====\nfirst\n\n===== z.txt =====\nlast\n\n",
                nested_path.display()
            )
        );

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

        let result = read_directory_with_options(
            &root,
            ReadOptions {
                exclude_readme: true,
                exclude_markdown: true,
            },
        )
        .unwrap();

        assert!(!result.contains("readme markdown"));
        assert!(!result.contains("readme text"));
        assert!(!result.contains("other markdown"));
        assert!(!result.contains("nested readme"));
        assert!(result.contains("keep"));

        fs::remove_dir_all(root).unwrap();
    }
}
