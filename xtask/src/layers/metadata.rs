//! Чтение `cargo metadata` и сбор из него списка крейтов и рёбер зависимостей.
//!
//! Архитектурно-зависимый крейт узнаётся по сегменту `aarch64` в пути, по `forced-target` на
//! этот триплет либо по ключу `metadata.molecular.arch`. Ключ нужен крейтам отделённых
//! workspace: `forced-target` задаёт триплет всем сборкам крейта, а такой крейт собирает чужой
//! workspace под свой таргет. Ключ читается и у пакета, и у workspace целиком.

use std::{
    collections::{BTreeMap, BTreeSet},
    fs,
    path::{Path, PathBuf},
    process::Command,
};

use anyhow::{Context, Result, bail};
use serde::Deserialize;

use crate::{layers::ARCH_SEGMENT, shell};

#[derive(Deserialize)]
pub(super) struct CargoMetadata {
    pub(super) packages: Vec<MetaPackage>,
    #[serde(default, rename = "metadata")]
    pub(super) workspace_metadata: Option<serde_json::Value>,
}

#[derive(Deserialize)]
pub(super) struct MetaPackage {
    pub(super) name: String,
    pub(super) manifest_path: PathBuf,
    pub(super) dependencies: Vec<MetaDependency>,
    #[serde(default)]
    pub(super) targets: Vec<MetaTarget>,
}

#[derive(Deserialize)]
pub(super) struct MetaDependency {
    pub(super) name: String,
    pub(super) path: Option<PathBuf>,
    #[serde(default)]
    pub(super) rename: Option<String>,
}

#[derive(Deserialize)]
pub(super) struct MetaTarget {
    pub(super) name: String,
    pub(super) kind: Vec<String>,
    pub(super) src_path: PathBuf,
}

#[derive(Clone, Debug, PartialEq, Eq, PartialOrd, Ord)]
pub(super) struct WorkspaceCrate {
    pub(super) name: String,
    pub(super) dir: String,
    pub(super) arch_specific: bool,
    pub(super) owns_process_state: bool,
    pub(super) links_std: bool,
}

#[derive(Clone, Debug, PartialEq, Eq, PartialOrd, Ord)]
pub(super) struct Edge {
    pub(super) from: String,
    pub(super) to: String,
}

pub(super) fn load_metadata(dir: &Path) -> Result<CargoMetadata> {
    let mut command = Command::new("cargo");
    command
        .args(["metadata", "--format-version", "1", "--no-deps"])
        .current_dir(dir);
    let metadata = shell::output(
        &mut command,
        &format!("cargo metadata in {}", dir.display()),
    )?;
    serde_json::from_str(&metadata).context("Failed to parse cargo metadata")
}

pub(super) fn collect_workspace(
    meta: &CargoMetadata,
    root: &Path,
    crates: &mut BTreeSet<WorkspaceCrate>,
    edges: &mut BTreeSet<Edge>,
) -> Result<()> {
    let workspace_arch = workspace_arch(meta) == Some(ARCH_SEGMENT);
    for pkg in &meta.packages {
        let manifest_dir = pkg
            .manifest_path
            .parent()
            .with_context(|| format!("crate {}: manifest path has no parent", pkg.name))?;
        let manifest = manifest_table(&pkg.manifest_path);
        crates.insert(WorkspaceCrate {
            name: pkg.name.clone(),
            dir: relative_dir(&pkg.name, manifest_dir, root)?,
            arch_specific: workspace_arch || arch_specific_manifest(manifest.as_ref()),
            owns_process_state: process_state_manifest(manifest.as_ref()),
            links_std: links_std(&pkg.targets)?,
        });
        // dependencies включает normal-, dev- и build-зависимости, path = Some отбирает
        // локальные. Дубликаты (normal + dev) схлопывает BTreeSet.
        for dep in &pkg.dependencies {
            let Some(dep_path) = &dep.path else { continue };
            // Цель path-зависимости попадает в список наравне с членами workspace: домен
            // крейта задает его расположение.
            crates.insert(WorkspaceCrate {
                name: dep.name.clone(),
                dir: relative_dir(&dep.name, dep_path, root)?,
                arch_specific: false,
                owns_process_state: false,
                links_std: false,
            });
            edges.insert(Edge {
                from: pkg.name.clone(),
                to: dep.name.clone(),
            });
        }
    }
    Ok(())
}

pub(super) fn merge_crates(crates: BTreeSet<WorkspaceCrate>) -> Vec<WorkspaceCrate> {
    // Пакет приходит с манифестом, цель path-зависимости - без него: признаки крейта
    // собираются по всем его вхождениям.
    let mut merged: BTreeMap<(String, String), (bool, bool, bool)> = BTreeMap::new();
    for krate in crates {
        let known = merged.entry((krate.name, krate.dir)).or_insert((
            krate.arch_specific,
            krate.owns_process_state,
            krate.links_std,
        ));
        known.0 |= krate.arch_specific;
        known.1 |= krate.owns_process_state;
        known.2 |= krate.links_std;
    }
    merged
        .into_iter()
        .map(
            |((name, dir), (arch_specific, owns_process_state, links_std))| WorkspaceCrate {
                name,
                dir,
                arch_specific,
                owns_process_state,
                links_std,
            },
        )
        .collect()
}

fn links_std(targets: &[MetaTarget]) -> Result<bool> {
    const CODE_KINDS: [&str; 5] = ["lib", "rlib", "dylib", "bin", "proc-macro"];

    for target in targets {
        // Сборочный скрипт и цели проверок идут на хосте и о форме программы ничего не говорят.
        if !target
            .kind
            .iter()
            .any(|kind| CODE_KINDS.contains(&kind.as_str()))
        {
            continue;
        }
        let source = fs::read_to_string(&target.src_path).with_context(|| {
            format!(
                "target {}: failed to read {}",
                target.name,
                target.src_path.display()
            )
        })?;
        // Крейт без std несёт внутренний атрибут `no_std`: голым либо под `cfg_attr`,
        // оставляющим std хостовым тестам.
        if !source
            .lines()
            .any(|line| line.trim_start().starts_with("#![") && line.contains("no_std"))
        {
            return Ok(true);
        }
    }
    Ok(false)
}

fn manifest_table(manifest: &Path) -> Option<toml::Table> {
    let text = fs::read_to_string(manifest).ok()?;
    text.parse::<toml::Table>().ok()
}

fn process_state_manifest(manifest: Option<&toml::Table>) -> bool {
    molecular_key(manifest, "process-state")
        .and_then(toml::Value::as_bool)
        .unwrap_or(false)
}

fn arch_specific_manifest(manifest: Option<&toml::Table>) -> bool {
    let forced = manifest
        .and_then(|table| table.get("package"))
        .and_then(toml::Value::as_table)
        .and_then(|package| package.get("forced-target"))
        .and_then(toml::Value::as_str)
        .is_some_and(|target| target.split('-').any(|segment| segment == ARCH_SEGMENT));
    forced || molecular_key(manifest, "arch").and_then(toml::Value::as_str) == Some(ARCH_SEGMENT)
}

fn workspace_arch(meta: &CargoMetadata) -> Option<&str> {
    meta.workspace_metadata
        .as_ref()?
        .get("molecular")?
        .get("arch")?
        .as_str()
}

fn molecular_key<'a>(manifest: Option<&'a toml::Table>, key: &str) -> Option<&'a toml::Value> {
    manifest?
        .get("package")
        .and_then(toml::Value::as_table)?
        .get("metadata")
        .and_then(toml::Value::as_table)?
        .get("molecular")
        .and_then(toml::Value::as_table)?
        .get(key)
}

pub(super) fn relative_dir(name: &str, dir: &Path, root: &Path) -> Result<String> {
    let rel = dir
        .strip_prefix(root)
        .with_context(|| format!("crate {name} is outside the workspace root"))?;
    let components: Vec<&str> = rel
        .components()
        .map(|c| {
            c.as_os_str()
                .to_str()
                .with_context(|| format!("crate {name}: non-UTF-8 path component"))
        })
        .collect::<Result<_>>()?;
    if components.is_empty() {
        bail!("crate {name}: empty relative path");
    }
    Ok(components.join("/"))
}

#[cfg(test)]
mod tests {
    use super::*;

    type PackageSpec<'a> = (&'a str, &'a str, &'a [(&'a str, &'a str)]);

    fn metadata(packages: &[PackageSpec<'_>]) -> CargoMetadata {
        CargoMetadata {
            packages: packages
                .iter()
                .map(|(name, dir, deps)| MetaPackage {
                    name: (*name).to_string(),
                    manifest_path: PathBuf::from(dir).join("Cargo.toml"),
                    dependencies: deps
                        .iter()
                        .map(|(dep_name, dep_dir)| MetaDependency {
                            name: (*dep_name).to_string(),
                            path: Some(PathBuf::from(dep_dir)),
                            rename: None,
                        })
                        .collect(),
                    targets: Vec::new(),
                })
                .collect(),
            workspace_metadata: None,
        }
    }

    fn edges(pairs: &[(&str, &str)]) -> Vec<Edge> {
        pairs
            .iter()
            .map(|(from, to)| Edge {
                from: (*from).to_string(),
                to: (*to).to_string(),
            })
            .collect()
    }

    #[test]
    fn workspace_arch_key_marks_every_member() {
        let mut meta = metadata(&[("a-program", "/repo/user/a-program", &[])]);
        meta.workspace_metadata = Some(serde_json::json!({ "molecular": { "arch": "aarch64" } }));
        let mut collected_crates = BTreeSet::new();
        let mut collected_edges = BTreeSet::new();
        collect_workspace(
            &meta,
            Path::new("/repo"),
            &mut collected_crates,
            &mut collected_edges,
        )
        .unwrap();

        let member = collected_crates
            .iter()
            .find(|krate| krate.name == "a-program")
            .expect("member is collected");
        assert!(member.arch_specific);
    }

    #[test]
    fn relative_dir_joins_components_below_root() {
        let dir = relative_dir("a-lib", Path::new("/repo/lib/a-lib"), Path::new("/repo")).unwrap();
        assert_eq!(dir, "lib/a-lib");
    }

    #[test]
    fn crate_outside_root_is_error() {
        let dir = relative_dir("stray", Path::new("/elsewhere/stray"), Path::new("/repo"));
        assert!(dir.is_err());
    }

    #[test]
    fn crate_at_root_is_error() {
        let dir = relative_dir("root", Path::new("/repo"), Path::new("/repo"));
        assert!(dir.is_err());
    }

    #[test]
    fn path_dependency_target_becomes_a_crate() {
        let meta = metadata(&[(
            "a-kernel",
            "/repo/kernel/a-kernel",
            &[("a-lib", "/repo/lib/a-lib")],
        )]);
        let mut collected_crates = BTreeSet::new();
        let mut collected_edges = BTreeSet::new();
        collect_workspace(
            &meta,
            Path::new("/repo"),
            &mut collected_crates,
            &mut collected_edges,
        )
        .unwrap();

        let dirs: Vec<(&str, &str)> = collected_crates
            .iter()
            .map(|krate| (krate.name.as_str(), krate.dir.as_str()))
            .collect();
        assert_eq!(
            dirs,
            [("a-kernel", "kernel/a-kernel"), ("a-lib", "lib/a-lib")]
        );
        assert_eq!(
            collected_edges.into_iter().collect::<Vec<_>>(),
            edges(&[("a-kernel", "a-lib")])
        );
    }

    #[test]
    fn path_dependency_outside_root_is_error() {
        let meta = metadata(&[(
            "a-kernel",
            "/repo/kernel/a-kernel",
            &[("vendored", "/vendor/vendored")],
        )]);
        let mut collected_crates = BTreeSet::new();
        let mut collected_edges = BTreeSet::new();
        assert!(
            collect_workspace(
                &meta,
                Path::new("/repo"),
                &mut collected_crates,
                &mut collected_edges,
            )
            .is_err()
        );
    }
}
