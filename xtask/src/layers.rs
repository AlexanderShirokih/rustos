//! Проверка правил workspace: домены крейтов, архитектурная разметка, профиль сборки каждого
//! workspace, копии таблиц корневого манифеста, описания проверок и сверка объявленных
//! зависимостей с кодом.
//!
//! Отделённые workspace перечисляет `workspace.exclude` корневого манифеста. Их крейты входят
//! в проверку наравне с членами корневого.

mod ci;
mod lints;
mod metadata;
mod profiles;
mod rules;
mod unused;

use std::{
    collections::BTreeSet,
    path::{Path, PathBuf},
};

use anyhow::{Context, Result, bail};

pub(crate) use crate::layers::{
    metadata::{ArchEntry, arch_registry, read_manifest, registered},
    profiles::Profile,
};

#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct Workspace {
    pub(crate) dir: PathBuf,
    pub(crate) profile: Profile,
}
use crate::{context::project_root, layers::metadata::Edge};

pub(crate) fn run() -> Result<()> {
    let mut crates = BTreeSet::new();
    let mut edges = BTreeSet::new();

    let root = project_root();
    let registry = arch_registry(&root)?;
    let detached = detached_workspaces(&root)?;

    let mut metas = Vec::new();
    let mut outside_profile = Vec::new();
    for workspace in std::iter::once(&root).chain(&detached) {
        let manifest = workspace.join("Cargo.toml");
        let profile = Profile::of_manifest(&read_manifest(&manifest)?, &manifest)?;
        let meta = metadata::load_metadata(workspace)?;
        let members =
            metadata::collect_workspace(&meta, &root, &registry, &mut crates, &mut edges)?;
        outside_profile.extend(profiles::check_members(
            profile, workspace, &members, &registry,
        ));
        metas.push(meta);
    }
    if !outside_profile.is_empty() {
        for entry in &outside_profile {
            eprintln!("{entry}");
        }
        bail!(
            "check-layers: {} crates outside the profile of their workspace",
            outside_profile.len()
        );
    }
    lints::check_lint_policy(&root, &detached)?;
    ci::check_descriptions(&root, &registry)?;

    let unused = unused::unused_dependencies(&metas, &root)?;
    if !unused.is_empty() {
        for entry in &unused {
            eprintln!("{entry}");
        }
        bail!("check-layers: {} unused path dependencies", unused.len());
    }

    let crates = metadata::merge_crates(crates)?;
    let edges: Vec<Edge> = edges.into_iter().collect();

    let violations = rules::check_layers(&crates, &edges)?;
    if !violations.is_empty() {
        for violation in &violations {
            eprintln!("{violation}");
        }
        bail!("check-layers: {} layer rule violations", violations.len());
    }
    println!(
        "check-layers: OK ({} crates, {} edges, {} detached workspaces, {} architectures)",
        crates.len(),
        edges.len(),
        detached.len(),
        registry.len()
    );
    Ok(())
}

pub(crate) fn detached_workspaces(root: &Path) -> Result<Vec<PathBuf>> {
    let manifest = root.join("Cargo.toml");
    let table = read_manifest(&manifest)?;
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
        let manifest = candidate.join("Cargo.toml");
        // Запись без таблицы `[workspace]` - пакет другого workspace, исключённый из корня
        // как его path-зависимость. Его проверяет workspace, который он объявляет.
        if manifest.is_file() && is_workspace_root(&read_manifest(&manifest)?) {
            detached.push(candidate);
        }
    }
    Ok(detached)
}

pub(crate) fn workspaces(root: &Path) -> Result<Vec<Workspace>> {
    let mut workspaces = Vec::new();
    for dir in std::iter::once(root.to_path_buf()).chain(detached_workspaces(root)?) {
        let manifest = dir.join("Cargo.toml");
        let profile = Profile::of_manifest(&read_manifest(&manifest)?, &manifest)?;
        workspaces.push(Workspace { dir, profile });
    }
    Ok(workspaces)
}

pub(crate) fn workspace_of_profile(root: &Path, profile: Profile) -> Result<PathBuf> {
    workspaces(root)?
        .into_iter()
        .find(|workspace| workspace.profile == profile)
        .map(|workspace| workspace.dir)
        .with_context(|| format!("no workspace declares profile {profile}"))
}

fn is_workspace_root(manifest: &toml::Table) -> bool {
    manifest.get("workspace").is_some_and(toml::Value::is_table)
}
