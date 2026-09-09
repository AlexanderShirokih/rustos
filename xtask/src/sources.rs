//! Чтение исходников крейта: рекурсивный обход каталога пакета.

use std::{fs, path::Path};

use anyhow::{Context, Result};

pub(crate) fn rust_sources(dir: &Path) -> Result<Vec<String>> {
    let mut sources = Vec::new();
    let mut stack = vec![dir.to_path_buf()];
    while let Some(current) = stack.pop() {
        let entries = fs::read_dir(&current)
            .with_context(|| format!("Failed to read {}", current.display()))?;
        for entry in entries {
            let entry = entry?;
            let path = entry.path();
            if entry.file_type()?.is_dir() {
                // В `target` лежат чужие сборочные артефакты, и крейт их не объявлял.
                if path.file_name().is_some_and(|name| name != "target") {
                    stack.push(path);
                }
            } else if path.extension().is_some_and(|ext| ext == "rs") {
                sources.push(
                    fs::read_to_string(&path)
                        .with_context(|| format!("Failed to read {}", path.display()))?,
                );
            }
        }
    }
    Ok(sources)
}
