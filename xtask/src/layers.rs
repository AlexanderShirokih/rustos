//! Проверка правил workspace: домены крейтов, архитектурная разметка, таблица линтов и
//! сверка объявленных зависимостей с кодом.
//!
//! Отделённые workspace перечисляет `workspace.exclude` корневого манифеста. Их крейты входят
//! в проверку наравне с членами корневого.

mod lints;
mod metadata;
mod rules;
mod unused;

use std::{
    collections::BTreeSet,
    fs,
    path::{Path, PathBuf},
};

use anyhow::{Context, Result, bail};

use crate::{context::project_root, layers::metadata::Edge};

pub(crate) const ARCH_SEGMENT: &str = "aarch64";

pub(crate) fn run() -> Result<()> {
    let mut crates = BTreeSet::new();
    let mut edges = BTreeSet::new();

    let root = project_root();
    let detached = detached_workspaces(&root)?;

    let mut metas = vec![metadata::load_metadata(&root)?];
    for workspace in &detached {
        metas.push(metadata::load_metadata(workspace)?);
    }
    for meta in &metas {
        metadata::collect_workspace(meta, &root, &mut crates, &mut edges)?;
    }
    lints::check_lint_policy(&root, &detached)?;

    let unused = unused::unused_dependencies(&metas, &root)?;
    if !unused.is_empty() {
        for entry in &unused {
            eprintln!("{entry}");
        }
        bail!("check-layers: {} unused path dependencies", unused.len());
    }

    let crates = metadata::merge_crates(crates);
    let edges: Vec<Edge> = edges.into_iter().collect();

    let violations = rules::check_layers(&crates, &edges)?;
    if !violations.is_empty() {
        for violation in &violations {
            eprintln!("{violation}");
        }
        bail!("check-layers: {} layer rule violations", violations.len());
    }
    println!(
        "check-layers: OK ({} crates, {} edges, {} detached workspaces)",
        crates.len(),
        edges.len(),
        detached.len()
    );
    Ok(())
}

pub(crate) fn detached_workspaces(root: &Path) -> Result<Vec<PathBuf>> {
    let manifest = root.join("Cargo.toml");
    let text = fs::read_to_string(&manifest)
        .with_context(|| format!("Failed to read {}", manifest.display()))?;
    let table = text
        .parse::<toml::Table>()
        .with_context(|| format!("Failed to parse {}", manifest.display()))?;
    let excluded = table
        .get("workspace")
        .and_then(toml::Value::as_table)
        .and_then(|workspace| workspace.get("exclude"))
        .and_then(toml::Value::as_array)
        .context("root manifest declares no workspace.exclude")?;

    let mut detached = Vec::new();
    for entry in excluded {
        let dir = entry
            .as_str()
            .context("workspace.exclude holds a non-string entry")?;
        let candidate = root.join(dir);
        if candidate.join("Cargo.toml").is_file() {
            detached.push(candidate);
        }
    }
    Ok(detached)
}
