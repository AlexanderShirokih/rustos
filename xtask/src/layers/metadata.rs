//! Чтение `cargo metadata`, сбор из него списка крейтов и рёбер зависимостей и реестр
//! архитектур корневого манифеста.
//!
//! Архитектура крейта - имя записи реестра, и узнаётся она по сегменту имени крейта или его
//! пути, по `forced-target`, равному bare-таргету записи, либо по ключу
//! `metadata.molecular.arch`. Ключ нужен крейтам отделённых workspace: `forced-target` задаёт
//! триплет всем сборкам крейта, а такой крейт собирает чужой workspace под свой таргет. Ключ
//! читается и у пакета, и у workspace целиком.

use std::{
    collections::{BTreeMap, BTreeSet},
    fs,
    path::{Path, PathBuf},
    process::Command,
};

use anyhow::{Context, Result, bail};
use serde::Deserialize;

use crate::shell;

#[derive(Clone, Debug, PartialEq, Eq, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct ArchEntry {
    pub(crate) name: String,
    pub(crate) bare: String,
    pub(crate) system: String,
}

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
    pub(super) arch: Option<String>,
    pub(super) owns_process_state: bool,
    pub(super) links_std: bool,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub(super) struct Member {
    pub(super) name: String,
    pub(super) links_std: bool,
    pub(super) proc_macro: bool,
    pub(super) forced_target: Option<String>,
}

#[derive(Clone, Debug, PartialEq, Eq, PartialOrd, Ord)]
pub(super) struct Edge {
    pub(super) from: String,
    pub(super) to: String,
}

pub(crate) fn read_manifest(manifest: &Path) -> Result<toml::Table> {
    let text = fs::read_to_string(manifest)
        .with_context(|| format!("Failed to read {}", manifest.display()))?;
    text.parse::<toml::Table>()
        .with_context(|| format!("Failed to parse {}", manifest.display()))
}

pub(crate) fn arch_registry(root: &Path) -> Result<Vec<ArchEntry>> {
    let manifest = root.join("Cargo.toml");
    parse_arch_registry(&read_manifest(&manifest)?)
        .with_context(|| format!("{}: architecture registry", manifest.display()))
}

fn parse_arch_registry(manifest: &toml::Table) -> Result<Vec<ArchEntry>> {
    let entries = manifest
        .get("workspace")
        .and_then(toml::Value::as_table)
        .and_then(|workspace| workspace.get("metadata"))
        .and_then(toml::Value::as_table)
        .and_then(|metadata| metadata.get("molecular"))
        .and_then(toml::Value::as_table)
        .and_then(|molecular| molecular.get("arch"))
        .context("no [[workspace.metadata.molecular.arch]] entries")?;
    let registry: Vec<ArchEntry> = entries
        .clone()
        .try_into()
        .context("malformed [[workspace.metadata.molecular.arch]] entry")?;
    if registry.is_empty() {
        bail!("the architecture registry is empty");
    }

    let mut names = BTreeSet::new();
    for entry in &registry {
        if !names.insert(entry.name.as_str()) {
            bail!("architecture `{}` is registered twice", entry.name);
        }
    }
    Ok(registry)
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
    registry: &[ArchEntry],
    crates: &mut BTreeSet<WorkspaceCrate>,
    edges: &mut BTreeSet<Edge>,
) -> Result<Vec<Member>> {
    let workspace_arch = workspace_arch(meta, registry)?;
    let mut members = Vec::new();
    for pkg in &meta.packages {
        let manifest_dir = pkg
            .manifest_path
            .parent()
            .with_context(|| format!("crate {}: manifest path has no parent", pkg.name))?;
        let manifest = read_manifest(&pkg.manifest_path).ok();
        let dir = relative_dir(&pkg.name, manifest_dir, root)?;
        let forced_target = forced_target(manifest.as_ref());
        let declared = declared_arch(
            &pkg.name,
            forced_target.as_deref(),
            manifest.as_ref(),
            registry,
        )?;
        crates.insert(WorkspaceCrate {
            name: pkg.name.clone(),
            dir: dir.clone(),
            arch: crate_arch(
                &pkg.name,
                &dir,
                registry,
                [workspace_arch.clone(), declared].into_iter().flatten(),
            )?,
            owns_process_state: process_state_manifest(manifest.as_ref()),
            links_std: links_std(&pkg.targets)?,
        });
        members.push(Member {
            name: pkg.name.clone(),
            links_std: links_std(&pkg.targets)?,
            proc_macro: is_proc_macro(&pkg.targets),
            forced_target,
        });
        // dependencies включает normal-, dev- и build-зависимости, path = Some отбирает
        // локальные. Дубликаты (normal + dev) схлопывает BTreeSet.
        for dep in &pkg.dependencies {
            let Some(dep_path) = &dep.path else { continue };
            // Цель path-зависимости попадает в список наравне с членами workspace: домен
            // крейта задает его расположение.
            let dep_dir = relative_dir(&dep.name, dep_path, root)?;
            crates.insert(WorkspaceCrate {
                name: dep.name.clone(),
                arch: crate_arch(&dep.name, &dep_dir, registry, [])?,
                dir: dep_dir,
                owns_process_state: false,
                links_std: false,
            });
            edges.insert(Edge {
                from: pkg.name.clone(),
                to: dep.name.clone(),
            });
        }
    }
    Ok(members)
}

pub(super) fn merge_crates(crates: BTreeSet<WorkspaceCrate>) -> Result<Vec<WorkspaceCrate>> {
    // Пакет приходит с манифестом, цель path-зависимости - без него. Признаки крейта
    // собираются по всем его вхождениям.
    let mut merged: BTreeMap<(String, String), WorkspaceCrate> = BTreeMap::new();
    for krate in crates {
        let key = (krate.name.clone(), krate.dir.clone());
        let Some(known) = merged.get_mut(&key) else {
            merged.insert(key, krate);
            continue;
        };
        match (&known.arch, &krate.arch) {
            (Some(known_arch), Some(arch)) if known_arch != arch => bail!(
                "crate {}: architecture `{known_arch}` conflicts with `{arch}`",
                krate.name
            ),
            (None, Some(arch)) => known.arch = Some(arch.clone()),
            _ => {}
        }
        known.owns_process_state |= krate.owns_process_state;
        known.links_std |= krate.links_std;
    }
    Ok(merged.into_values().collect())
}

fn crate_arch(
    name: &str,
    dir: &str,
    registry: &[ArchEntry],
    declared: impl IntoIterator<Item = String>,
) -> Result<Option<String>> {
    let mut arch: Option<String> = registry
        .iter()
        .map(|entry| entry.name.as_str())
        .find(|arch| {
            name.split('-').any(|segment| segment == *arch)
                || dir.split(['/', '-']).any(|segment| segment == *arch)
        })
        .map(str::to_owned);
    for value in declared {
        match &arch {
            Some(known) if known != &value => {
                bail!("crate {name}: architecture `{known}` conflicts with `{value}`")
            }
            Some(_) => {}
            None => arch = Some(value),
        }
    }
    Ok(arch)
}

fn declared_arch(
    name: &str,
    forced_target: Option<&str>,
    manifest: Option<&toml::Table>,
    registry: &[ArchEntry],
) -> Result<Option<String>> {
    let mut declared = None;
    if let Some(target) = forced_target {
        let entry = registry
            .iter()
            .find(|entry| entry.bare == target)
            .with_context(|| {
                format!("crate {name}: forced-target `{target}` names no registered architecture")
            })?;
        declared = Some(entry.name.clone());
    }
    if let Some(value) = molecular_key(manifest, "arch").and_then(toml::Value::as_str) {
        let entry = registered(value, registry)
            .with_context(|| format!("crate {name}: metadata.molecular.arch"))?;
        match &declared {
            Some(known) if known != &entry.name => bail!(
                "crate {name}: forced-target names `{known}`, metadata.molecular.arch names `{}`",
                entry.name
            ),
            _ => declared = Some(entry.name.clone()),
        }
    }
    Ok(declared)
}

pub(crate) fn registered<'a>(name: &str, registry: &'a [ArchEntry]) -> Result<&'a ArchEntry> {
    registry
        .iter()
        .find(|entry| entry.name == name)
        .with_context(|| {
            let known: Vec<&str> = registry.iter().map(|entry| entry.name.as_str()).collect();
            format!(
                "architecture `{name}` is not registered; registered: {}",
                known.join(", ")
            )
        })
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
        // Крейт без std содержит внутренний атрибут `no_std`: голым либо под `cfg_attr`,
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

fn is_proc_macro(targets: &[MetaTarget]) -> bool {
    targets
        .iter()
        .any(|target| target.kind.iter().any(|kind| kind == "proc-macro"))
}

fn process_state_manifest(manifest: Option<&toml::Table>) -> bool {
    molecular_key(manifest, "process-state")
        .and_then(toml::Value::as_bool)
        .unwrap_or(false)
}

fn forced_target(manifest: Option<&toml::Table>) -> Option<String> {
    manifest?
        .get("package")
        .and_then(toml::Value::as_table)?
        .get("forced-target")
        .and_then(toml::Value::as_str)
        .map(str::to_owned)
}

fn workspace_arch(meta: &CargoMetadata, registry: &[ArchEntry]) -> Result<Option<String>> {
    let Some(value) = meta
        .workspace_metadata
        .as_ref()
        .and_then(|metadata| metadata.get("molecular")?.get("arch")?.as_str())
    else {
        return Ok(None);
    };
    let entry = registered(value, registry).context("workspace.metadata.molecular.arch")?;
    Ok(Some(entry.name.clone()))
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

    fn registry() -> Vec<ArchEntry> {
        vec![ArchEntry {
            name: "aarch64".to_string(),
            bare: "aarch64-unknown-none".to_string(),
            system: "targets/aarch64-unknown-molecular.json".to_string(),
        }]
    }

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

    fn collect(meta: &CargoMetadata) -> Result<(BTreeSet<WorkspaceCrate>, BTreeSet<Edge>)> {
        let mut crates = BTreeSet::new();
        let mut edges = BTreeSet::new();
        collect_workspace(
            meta,
            Path::new("/repo"),
            &registry(),
            &mut crates,
            &mut edges,
        )?;
        Ok((crates, edges))
    }

    fn registry_of(text: &str) -> Result<Vec<ArchEntry>> {
        parse_arch_registry(&text.parse::<toml::Table>()?)
    }

    #[test]
    fn a_repeated_name_is_refused() {
        let text = "
[[workspace.metadata.molecular.arch]]
name = \"aarch64\"
bare = \"aarch64-unknown-none\"
system = \"targets/aarch64-unknown-molecular.json\"

[[workspace.metadata.molecular.arch]]
name = \"aarch64\"
bare = \"aarch64-unknown-none-softfloat\"
system = \"targets/aarch64-unknown-molecular.json\"
";
        let error = registry_of(text).unwrap_err();
        assert!(error.to_string().contains("registered twice"), "{error}");
    }

    #[test]
    fn workspace_arch_key_marks_every_member() {
        let mut meta = metadata(&[("a-program", "/repo/user/a-program", &[])]);
        meta.workspace_metadata = Some(serde_json::json!({ "molecular": { "arch": "aarch64" } }));
        let (collected_crates, _) = collect(&meta).unwrap();

        let member = collected_crates
            .iter()
            .find(|krate| krate.name == "a-program")
            .expect("member is collected");
        assert_eq!(member.arch.as_deref(), Some("aarch64"));
    }

    #[test]
    fn the_arch_segment_of_the_path_names_the_registered_arch() {
        let registry = registry();
        assert_eq!(
            crate_arch("hal", "kernel/hal-aarch64", &registry, []).unwrap(),
            Some("aarch64".to_string())
        );
        assert_eq!(
            crate_arch("a-lib", "lib/a-lib", &registry, []).unwrap(),
            None
        );
        // Сегмент вне реестра архитектурой не считается, так как имя её называть не обязано.
        assert_eq!(
            crate_arch("hal-riscv64", "kernel/hal-riscv64", &registry, []).unwrap(),
            None
        );
    }

    #[test]
    fn a_declared_arch_that_contradicts_the_path_is_error() {
        let mut registry = registry();
        registry.push(ArchEntry {
            name: "riscv64".to_string(),
            bare: "riscv64gc-unknown-none-elf".to_string(),
            system: "targets/riscv64-unknown-molecular.json".to_string(),
        });
        let error = crate_arch(
            "hal-riscv64",
            "kernel/hal-riscv64",
            &registry,
            ["aarch64".to_string()],
        )
        .unwrap_err();
        assert!(error.to_string().contains("conflicts"), "{error}");
    }

    #[test]
    fn a_second_registered_architecture_is_read_from_the_name_and_the_forced_target() {
        let mut registry = registry();
        registry.push(ArchEntry {
            name: "riscv64".to_string(),
            bare: "riscv64gc-unknown-none-elf".to_string(),
            system: "targets/riscv64-unknown-molecular.json".to_string(),
        });
        let declared = declared_arch(
            "hal-riscv64",
            Some("riscv64gc-unknown-none-elf"),
            None,
            &registry,
        )
        .unwrap();
        assert_eq!(declared.as_deref(), Some("riscv64"));
        let arch = crate_arch("hal-riscv64", "kernel/hal-riscv64", &registry, declared).unwrap();
        assert_eq!(arch.as_deref(), Some("riscv64"));
    }

    #[test]
    fn a_forced_target_outside_the_registry_is_error() {
        let error = declared_arch(
            "hal-riscv64",
            Some("riscv64gc-unknown-none-elf"),
            None,
            &registry(),
        )
        .unwrap_err();
        assert!(
            error.to_string().contains("riscv64gc-unknown-none-elf"),
            "{error}"
        );
    }

    #[test]
    fn merging_two_architectures_of_one_crate_is_error() {
        let krate = |arch: &str| WorkspaceCrate {
            name: "hal".to_string(),
            dir: "kernel/hal".to_string(),
            arch: Some(arch.to_string()),
            owns_process_state: false,
            links_std: false,
        };
        let crates: BTreeSet<WorkspaceCrate> = [krate("aarch64"), krate("riscv64")].into();
        assert!(merge_crates(crates).is_err());
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
        let (collected_crates, collected_edges) = collect(&meta).unwrap();

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
        assert!(collect(&meta).is_err());
    }
}
