//! Реестр закреплённого стороннего кода. Запись - исходники, из которых собираются программы
//! userland, или данные образа. Каталог записи - `target/third-party/<name>`.
//!
//! Данные записи готовятся, когда источник файла образа лежит в её каталоге.

use std::path::{Path, PathBuf};

use anyhow::Result;

use crate::{
    context::project_root,
    fetch::{self, Format, Pin},
};

const DIR: &str = "target/third-party";

struct Entry {
    name: &'static str,
    role: Role,
    pin: Pin,
}

enum Role {
    Sources,
    Data,
}

const ENTRIES: &[Entry] = &[
    Entry {
        name: "doomgeneric",
        role: Role::Sources,
        pin: Pin::GitCommit {
            repo: "https://github.com/ozkl/doomgeneric",
            commit: "dcb7a8dbc7a16ce3dda29382ac9aae9d77d21284",
        },
    },
    Entry {
        name: "freedoom",
        role: Role::Data,
        pin: Pin::Archive {
            url: "https://github.com/freedoom/freedoom/releases/download/v0.13.0/freedoom-0.13.0.zip",
            sha256: "3f9b264f3e3ce503b4fb7f6bdcb1f419d93c7b546f4df3e874dd878db9688f59",
            format: Format::Zip,
        },
    },
];

pub(crate) fn materialize_sources() -> Result<()> {
    let root = project_root();
    ENTRIES
        .iter()
        .filter(|entry| matches!(entry.role, Role::Sources))
        .try_for_each(|entry| materialize(&root, entry, false).map(drop))
}

pub(crate) fn materialize_referenced(sources: &[PathBuf]) -> Result<()> {
    let root = project_root();
    ENTRIES
        .iter()
        .filter(|entry| matches!(entry.role, Role::Data))
        .filter(|entry| {
            sources
                .iter()
                .any(|source| references(&root, source, entry.name))
        })
        .try_for_each(|entry| materialize(&root, entry, false).map(drop))
}

pub(crate) fn run(force: bool) -> Result<()> {
    let root = project_root();
    for entry in ENTRIES {
        let dir = materialize(&root, entry, force)?;
        println!("third-party: {}", dir.display());
    }
    Ok(())
}

fn materialize(root: &Path, entry: &Entry, force: bool) -> Result<PathBuf> {
    let dir = root.join(DIR).join(entry.name);
    fetch::materialize(entry.name, &dir, &entry.pin, force)
}

fn references(root: &Path, source: &Path, name: &str) -> bool {
    root.join(source)
        .strip_prefix(root.join(DIR))
        .ok()
        .and_then(|rest| rest.components().next())
        .is_some_and(|first| first.as_os_str() == name)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_source_references_the_entry_named_by_its_first_component() {
        let root = Path::new("/project");
        let referenced = |source: &str| references(root, Path::new(source), "data");

        assert!(referenced("target/third-party/data/set/a.bin"));
        assert!(referenced("/project/target/third-party/data/set/*.bin"));
        assert!(!referenced("target/third-party/data-extra/a.bin"));
        assert!(!referenced("target/third-party/other/data/a.bin"));
        assert!(!referenced("user/data/a.bin"));
    }
}
