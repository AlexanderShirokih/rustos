//! Форматирование дерева целиком. `cargo fmt --all` видит только членов своего workspace, а
//! часть дерева живёт вне корневого: список отделённых берётся из `workspace.exclude`
//! корневого манифеста, второго реестра в дереве нет.

use std::{path::Path, process::Command};

use anyhow::{Context, Result, bail};

use crate::{context::project_root, layers::detached_workspaces};

pub(crate) fn run(check: bool) -> Result<()> {
    let root = project_root();
    let mut manifests = vec![root.join("Cargo.toml")];
    for workspace in detached_workspaces(&root)? {
        manifests.push(workspace.join("Cargo.toml"));
    }

    let mut unformatted = Vec::new();
    for manifest in &manifests {
        if !format_workspace(manifest, check)? {
            unformatted.push(manifest.clone());
        }
    }
    if !unformatted.is_empty() {
        for manifest in &unformatted {
            eprintln!("fmt: {} needs formatting", manifest.display());
        }
        bail!(
            "fmt: {} of {} workspaces need formatting; run `cargo xtask fmt`",
            unformatted.len(),
            manifests.len()
        );
    }
    println!("fmt: OK ({} workspaces)", manifests.len());
    Ok(())
}

fn format_workspace(manifest: &Path, check: bool) -> Result<bool> {
    let mut command = Command::new("cargo");
    command
        .arg("fmt")
        .arg("--manifest-path")
        .arg(manifest)
        .arg("--all");
    if check {
        command.arg("--check");
    }
    let status = command
        .status()
        .with_context(|| format!("Failed to run cargo fmt for {}", manifest.display()))?;
    // Под `--check` ненулевой код отвечает и неотформатированному дереву, и отказу самого
    // rustfmt. Без него ненулевой код означает отказ.
    if !check && !status.success() {
        bail!("cargo fmt over {} failed with {status}", manifest.display());
    }
    Ok(status.success())
}
