use std::{
    collections::BTreeMap,
    path::{Path, PathBuf},
    process::Command,
};

use anyhow::{Context as _, Result, bail};

use crate::scope::{Domain, FileMeta};

pub(crate) struct Workspace {
    /// Корень репозитория.
    pub root: PathBuf,
    packages: BTreeMap<PathBuf, String>,
}

impl Workspace {
    pub(crate) fn discover(from: &Path) -> Result<Workspace> {
        let root = git(from, &["rev-parse", "--show-toplevel"])?;
        let root = PathBuf::from(root.trim());
        let listing = git(&root, &["ls-files", "-z", "--", "*Cargo.toml"])?;

        let mut packages = BTreeMap::new();
        for entry in listing.split('\0').filter(|item| !item.is_empty()) {
            let manifest = root.join(entry);
            let Ok(text) = std::fs::read_to_string(&manifest) else {
                continue;
            };
            let Ok(table) = text.parse::<toml::Table>() else {
                continue;
            };
            let Some(name) = table
                .get("package")
                .and_then(toml::Value::as_table)
                .and_then(|package| package.get("name"))
                .and_then(toml::Value::as_str)
            else {
                continue;
            };
            let Some(directory) = Path::new(entry).parent() else {
                continue;
            };
            packages.insert(directory.to_path_buf(), name.to_owned());
        }

        Ok(Workspace { root, packages })
    }

    pub(crate) fn files(&self, untracked: bool) -> Result<Vec<PathBuf>> {
        let args: &[&str] = if untracked {
            &["ls-files", "-z", "-c", "-o", "--exclude-standard"]
        } else {
            &["ls-files", "-z"]
        };
        let listing = git(&self.root, args)?;
        let mut files: Vec<PathBuf> = listing
            .split('\0')
            .filter(|entry| !entry.is_empty())
            .map(PathBuf::from)
            .collect();
        files.sort();
        files.dedup();
        Ok(files)
    }

    pub(crate) fn meta(&self, path: &Path) -> FileMeta {
        let package = self
            .packages
            .iter()
            .filter(|(directory, _)| path.starts_with(directory))
            .max_by_key(|(directory, _)| directory.components().count())
            .map(|(_, name)| name.clone());

        FileMeta {
            path: path.to_path_buf(),
            package,
            domain: Domain::of(path),
        }
    }
}

pub(crate) fn git(directory: &Path, args: &[&str]) -> Result<String> {
    let output = Command::new("git")
        .args(args)
        .current_dir(directory)
        .output()
        .with_context(|| format!("run git {}", args.join(" ")))?;
    if !output.status.success() {
        bail!(
            "git {} failed: {}",
            args.join(" "),
            String::from_utf8_lossy(&output.stderr).trim()
        );
    }
    Ok(String::from_utf8_lossy(&output.stdout).into_owned())
}
