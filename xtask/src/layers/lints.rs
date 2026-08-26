//! Сверка таблицы линтов отделённых workspace с корневой: наследовать её вне корневого
//! дерева нечем.

use std::{
    fs,
    path::{Path, PathBuf},
};

use anyhow::{Context, Result, bail};

pub(super) fn check_lint_policy(root: &Path, detached: &[PathBuf]) -> Result<()> {
    let expected = workspace_lints(&root.join("Cargo.toml"))?;
    for workspace in detached {
        let manifest = workspace.join("Cargo.toml");
        if workspace_lints(&manifest)? != expected {
            bail!(
                "check-layers: {} declares a [workspace.lints] table that differs from the root one",
                manifest.display()
            );
        }
    }
    Ok(())
}

fn workspace_lints(manifest: &Path) -> Result<toml::Value> {
    let text = fs::read_to_string(manifest)
        .with_context(|| format!("Failed to read {}", manifest.display()))?;
    let table = text
        .parse::<toml::Table>()
        .with_context(|| format!("Failed to parse {}", manifest.display()))?;
    table
        .get("workspace")
        .and_then(toml::Value::as_table)
        .and_then(|workspace| workspace.get("lints"))
        .cloned()
        .with_context(|| format!("{} declares no [workspace.lints]", manifest.display()))
}
