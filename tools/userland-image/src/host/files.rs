use std::{
    collections::HashMap,
    fs,
    path::{Path, PathBuf},
    str::FromStr,
};

use anyhow::{Context, Result, anyhow, bail, ensure};

use super::{glob::glob_matches, image::validate_image_path, source::ImageFile};

const WILDCARDS: [char; 2] = ['*', '?'];

/// Файл, добавляемый в образ поверх манифеста: путь записи и источник на диске
/// относительно корня проекта. Разбирается из аргумента вида `path=source`.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ExtraFile {
    pub path: String,
    pub source: PathBuf,
}

#[derive(Clone, Debug)]
pub(super) struct LoadedFile {
    pub(super) path: String,
    pub(super) source: PathBuf,
    pub(super) bytes: Vec<u8>,
}

#[derive(Clone, Debug)]
struct DirectoryFile {
    relative: String,
    source: PathBuf,
}

struct SourcePattern<'a> {
    root: &'a str,
    glob: &'a str,
}

#[derive(Debug, Default)]
struct FileSet {
    files: Vec<LoadedFile>,
    positions: HashMap<String, usize>,
}

impl FromStr for ExtraFile {
    type Err = anyhow::Error;

    fn from_str(argument: &str) -> Result<Self> {
        let (path, source) = argument
            .split_once('=')
            .ok_or_else(|| anyhow!("image file '{argument}' must be given as 'path=source'"))?;
        ensure!(
            !path.is_empty() && !source.is_empty(),
            "image file '{argument}' must set both a non-empty path and a non-empty source"
        );
        Ok(Self {
            path: path.to_string(),
            source: PathBuf::from(source),
        })
    }
}

impl FileSet {
    fn insert(&mut self, file: LoadedFile) -> Result<()> {
        if let Some(&position) = self.positions.get(&file.path) {
            let present = self
                .files
                .get(position)
                .context("image file index lost its file")?;
            ensure!(
                present.bytes == file.bytes,
                "image path '{}' is filled by '{}' and '{}' with different content",
                file.path,
                present.source.display(),
                file.source.display()
            );
            return Ok(());
        }
        self.positions.insert(file.path.clone(), self.files.len());
        self.files.push(file);
        Ok(())
    }

    fn into_files(self) -> Vec<LoadedFile> {
        self.files
    }
}

pub(super) fn collect_files(
    project_root: &Path,
    manifest_files: &[ImageFile],
    extra_files: &[ExtraFile],
) -> Result<Vec<LoadedFile>> {
    let mut files = FileSet::default();
    for file in manifest_files {
        for loaded in load_source(&file.path, project_root, &file.source)? {
            files.insert(loaded)?;
        }
    }
    for file in extra_files {
        for loaded in load_source(&file.path, project_root, &file.source)? {
            files.insert(loaded)?;
        }
    }
    Ok(files.into_files())
}

fn load_source(path: &str, project_root: &Path, source: &Path) -> Result<Vec<LoadedFile>> {
    validate_image_path(path)?;
    let text = source
        .to_str()
        .ok_or_else(|| anyhow!("image source '{}' is not UTF-8", source.display()))?;
    let Some(pattern) = split_pattern(text) else {
        let source = project_root.join(source);
        ensure!(
            !source.is_dir(),
            "image source '{}' of path '{path}' is a directory; \
             its files are selected by a pattern such as '{text}/**'",
            source.display()
        );
        return Ok(vec![load_file(path, source)?]);
    };

    // Корень образа префикса не даёт, остальные пути дают себя целиком.
    let prefix = path.trim_end_matches('/');
    let mut found = Vec::new();
    collect_directory_files(&project_root.join(pattern.root), "", &mut found)?;
    found
        .into_iter()
        .filter(|file| glob_matches(pattern.glob, &file.relative))
        .map(|file| load_file(&format!("{prefix}/{}", file.relative), file.source))
        .collect()
}

fn split_pattern(source: &str) -> Option<SourcePattern<'_>> {
    let mut cursor = 0;
    for component in source.split('/') {
        if component.contains(WILDCARDS) {
            return Some(SourcePattern {
                root: source[..cursor].trim_end_matches('/'),
                glob: &source[cursor..],
            });
        }
        cursor += component.len() + 1;
    }
    None
}

fn load_file(path: &str, source: PathBuf) -> Result<LoadedFile> {
    validate_image_path(path)?;
    let bytes = fs::read(&source).with_context(|| {
        format!(
            "Failed to read source of image file '{}' from '{}'",
            path,
            source.display()
        )
    })?;
    Ok(LoadedFile {
        path: path.to_string(),
        source,
        bytes,
    })
}

fn collect_directory_files(
    directory: &Path,
    prefix: &str,
    found: &mut Vec<DirectoryFile>,
) -> Result<()> {
    let read_dir = fs::read_dir(directory)
        .with_context(|| format!("Failed to read image directory '{}'", directory.display()))?;
    let mut entries = read_dir
        .collect::<Result<Vec<_>, _>>()
        .with_context(|| format!("Failed to read image directory '{}'", directory.display()))?;
    entries.sort_by_key(std::fs::DirEntry::file_name);

    for entry in entries {
        let source = entry.path();
        let name = entry
            .file_name()
            .into_string()
            .map_err(|_| anyhow!("image directory entry '{}' is not UTF-8", source.display()))?;
        let relative = if prefix.is_empty() {
            name
        } else {
            format!("{prefix}/{name}")
        };
        let file_type = entry.file_type().with_context(|| {
            format!("Failed to read type of image entry '{}'", source.display())
        })?;

        if file_type.is_dir() {
            collect_directory_files(&source, &relative, found)?;
        } else if file_type.is_file() {
            found.push(DirectoryFile { relative, source });
        } else {
            bail!(
                "image directory entry '{}' is neither a file nor a directory",
                source.display()
            );
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use tempfile::TempDir;

    use super::*;

    fn image_file(path: &str, source: &str) -> ImageFile {
        ImageFile {
            path: path.to_string(),
            source: PathBuf::from(source),
        }
    }

    fn manifest_files(path: &str, source: &str) -> Vec<ImageFile> {
        vec![image_file(path, source)]
    }

    fn source_tree(files: &[(&str, &[u8])]) -> TempDir {
        let root = TempDir::new().expect("create source directory");
        for (path, bytes) in files {
            let path = root.path().join(path);
            fs::create_dir_all(path.parent().expect("source path has a parent"))
                .expect("create source directories");
            fs::write(path, bytes).expect("write source file");
        }
        root
    }

    fn collected_paths(files: &[LoadedFile]) -> Vec<&str> {
        files.iter().map(|file| file.path.as_str()).collect()
    }

    #[test]
    fn directory_files_are_collected_recursively_in_name_order() {
        let tree = source_tree(&[
            ("assets/b.txt", b"b"),
            ("assets/a.txt", b"a"),
            ("assets/nested/c.txt", b"c"),
        ]);
        let manifest = manifest_files("/data", "assets/**");

        let files = collect_files(tree.path(), &manifest, &[]).expect("collect must succeed");
        assert_eq!(
            collected_paths(&files),
            ["/data/a.txt", "/data/b.txt", "/data/nested/c.txt"]
        );
        assert_eq!(files[2].bytes, b"c");
    }

    #[test]
    fn directory_glob_selects_the_files_that_enter_the_image() {
        let tree = source_tree(&[
            ("assets/notes.md", b"md"),
            ("assets/sample.txt", b"txt"),
            ("assets/nested/deeper.txt", b"deeper"),
        ]);
        let manifest = manifest_files("/data", "assets/**/*.txt");

        let files = collect_files(tree.path(), &manifest, &[]).expect("collect must succeed");
        assert_eq!(
            collected_paths(&files),
            ["/data/nested/deeper.txt", "/data/sample.txt"]
        );
    }

    #[test]
    fn a_star_selects_the_files_of_one_directory() {
        let tree = source_tree(&[
            ("assets/sample.txt", b"txt"),
            ("assets/nested/deeper.txt", b"deeper"),
        ]);
        let manifest = manifest_files("/data", "assets/*");

        let files = collect_files(tree.path(), &manifest, &[]).expect("collect must succeed");
        assert_eq!(collected_paths(&files), ["/data/sample.txt"]);
    }

    #[test]
    fn a_directory_source_without_a_pattern_is_rejected() {
        let tree = source_tree(&[("assets/sample.txt", b"txt")]);
        let manifest = manifest_files("/data/assets", "assets");

        let err = collect_files(tree.path(), &manifest, &[])
            .expect_err("directory without a pattern must fail");
        assert!(err.to_string().ends_with(
            "of path '/data/assets' is a directory; \
             its files are selected by a pattern such as 'assets/**'"
        ));
    }

    #[test]
    fn a_source_splits_at_its_first_wildcard() {
        let pattern =
            split_pattern("user/rootkeeper/assets/**/*.txt").expect("source has a pattern");
        assert_eq!(pattern.root, "user/rootkeeper/assets");
        assert_eq!(pattern.glob, "**/*.txt");

        let pattern = split_pattern("assets/*").expect("source has a pattern");
        assert_eq!(pattern.root, "assets");
        assert_eq!(pattern.glob, "*");

        assert!(split_pattern("user/rootkeeper/sample.txt").is_none());
    }

    #[test]
    fn directory_under_the_image_root_keeps_the_relative_path() {
        let tree = source_tree(&[("assets/boot/sample", b"hi")]);
        let manifest = manifest_files("/", "assets/**");

        let files = collect_files(tree.path(), &manifest, &[]).expect("collect must succeed");
        assert_eq!(collected_paths(&files), ["/boot/sample"]);
    }

    #[test]
    fn command_line_files_join_the_files_of_the_manifest() {
        let tree = source_tree(&[("assets/sample.txt", b"txt"), ("motd", b"welcome")]);
        let extra = [ExtraFile {
            path: "/etc/motd".to_string(),
            source: PathBuf::from("motd"),
        }];
        let manifest = manifest_files("/data", "assets/**");

        let files = collect_files(tree.path(), &manifest, &extra).expect("collect must succeed");
        assert_eq!(collected_paths(&files), ["/data/sample.txt", "/etc/motd"]);
        assert_eq!(files[1].bytes, b"welcome");
    }

    #[test]
    fn one_path_filled_with_equal_content_gives_one_record() {
        let tree = source_tree(&[("assets/sample.txt", b"same")]);
        let mut manifest = manifest_files("/data", "assets/**");
        manifest.push(image_file("/data/sample.txt", "assets/sample.txt"));
        let extra = [ExtraFile {
            path: "/data/sample.txt".to_string(),
            source: PathBuf::from("assets/sample.txt"),
        }];

        let files = collect_files(tree.path(), &manifest, &extra).expect("collect must succeed");
        assert_eq!(collected_paths(&files), ["/data/sample.txt"]);
    }

    #[test]
    fn one_path_filled_with_different_content_is_rejected() {
        let tree = source_tree(&[("assets/sample.txt", b"tree"), ("other.txt", b"other")]);
        let mut manifest = manifest_files("/data", "assets/**");
        manifest.push(image_file("/data/sample.txt", "other.txt"));

        let err =
            collect_files(tree.path(), &manifest, &[]).expect_err("conflicting content must fail");
        assert!(
            err.to_string()
                .starts_with("image path '/data/sample.txt' is filled by")
        );
        assert!(err.to_string().ends_with("with different content"));
    }

    #[test]
    fn command_line_file_parses_path_and_source() {
        let file: ExtraFile = "/etc/motd=user/rootkeeper/motd.txt"
            .parse()
            .expect("argument must parse");
        assert_eq!(file.path, "/etc/motd");
        assert_eq!(file.source, PathBuf::from("user/rootkeeper/motd.txt"));
    }

    #[test]
    fn command_line_file_without_a_source_is_rejected() {
        let err = "/etc/motd"
            .parse::<ExtraFile>()
            .expect_err("argument without a source must fail");
        assert_eq!(
            err.to_string(),
            "image file '/etc/motd' must be given as 'path=source'"
        );

        let err = "/etc/motd="
            .parse::<ExtraFile>()
            .expect_err("empty source must fail");
        assert_eq!(
            err.to_string(),
            "image file '/etc/motd=' must set both a non-empty path and a non-empty source"
        );
    }
}
