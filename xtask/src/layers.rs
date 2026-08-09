//! Проверка правил workspace: домен крейта задается его верхней директорией, ребра
//! зависимостей сверяются с таблицей разрешений. Архитектурно-зависимые крейты (сегмент
//! `aarch64` в пути, `forced-target` на этот триплет либо ключ `metadata.molecular.arch`)
//! доступны только другим архитектурно-зависимым крейтам.
//!
//! Ключ нужен крейтам отделенных workspace: `forced-target` задает триплет всем сборкам крейта,
//! а такой крейт собирает чужой workspace под свой таргет. Ключ читается и у пакета, и у
//! workspace целиком.
//!
//! Отделенные workspace перечисляет `workspace.exclude` корневого манифеста. Их крейты входят в
//! проверку наравне с членами корневого, а таблица линтов каждого сверяется с корневой:
//! наследовать ее вне корневого дерева нечем.
//!
//! Заодно сверяется, что каждое объявленное ребро есть и в коде: манифест задает граф, по
//! которому идет проверка слоев, и ребро без единой ссылки в исходниках этот граф искажает.

use std::{
    collections::{BTreeMap, BTreeSet},
    fmt, fs,
    path::{Path, PathBuf},
    process::Command,
};

use anyhow::{Context, Result, bail};
use serde::Deserialize;

use crate::context::project_root;

const ARCH_SEGMENT: &str = "aarch64";

#[derive(Deserialize)]
struct CargoMetadata {
    packages: Vec<MetaPackage>,
    #[serde(default, rename = "metadata")]
    workspace_metadata: Option<serde_json::Value>,
}

#[derive(Deserialize)]
struct MetaPackage {
    name: String,
    manifest_path: PathBuf,
    dependencies: Vec<MetaDependency>,
    #[serde(default)]
    targets: Vec<MetaTarget>,
}

#[derive(Deserialize)]
struct MetaDependency {
    name: String,
    path: Option<PathBuf>,
    #[serde(default)]
    rename: Option<String>,
}

#[derive(Deserialize)]
struct MetaTarget {
    name: String,
    kind: Vec<String>,
}

#[derive(Clone, Debug, PartialEq, Eq, PartialOrd, Ord)]
pub(crate) struct WorkspaceCrate {
    pub(crate) name: String,
    pub(crate) dir: String,
    pub(crate) arch_specific: bool,
}

#[derive(Clone, Debug, PartialEq, Eq, PartialOrd, Ord)]
pub(crate) struct Edge {
    pub(crate) from: String,
    pub(crate) to: String,
}

pub(crate) fn run() -> Result<()> {
    let mut crates = BTreeSet::new();
    let mut edges = BTreeSet::new();

    let root = project_root();
    let detached = detached_workspaces(&root)?;

    let mut metas = vec![load_metadata(&root)?];
    for workspace in &detached {
        metas.push(load_metadata(workspace)?);
    }
    for meta in &metas {
        collect_workspace(meta, &root, &mut crates, &mut edges)?;
    }
    check_lint_policy(&root, &detached)?;

    let unused = unused_dependencies(&metas, &root)?;
    if !unused.is_empty() {
        for entry in &unused {
            eprintln!("{entry}");
        }
        bail!("check-layers: {} unused path dependencies", unused.len());
    }

    let crates = merge_crates(crates);
    let edges: Vec<Edge> = edges.into_iter().collect();

    let violations = check_layers(&crates, &edges)?;
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

fn check_lint_policy(root: &Path, detached: &[PathBuf]) -> Result<()> {
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

fn detached_workspaces(root: &Path) -> Result<Vec<PathBuf>> {
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

fn load_metadata(dir: &Path) -> Result<CargoMetadata> {
    let output = Command::new("cargo")
        .args(["metadata", "--format-version", "1", "--no-deps"])
        .current_dir(dir)
        .output()
        .with_context(|| format!("Failed to run cargo metadata in {}", dir.display()))?;
    if !output.status.success() {
        bail!(
            "cargo metadata in {} failed with {}",
            dir.display(),
            output.status
        );
    }
    serde_json::from_slice(&output.stdout).context("Failed to parse cargo metadata")
}

fn collect_workspace(
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
        crates.insert(WorkspaceCrate {
            name: pkg.name.clone(),
            dir: relative_dir(&pkg.name, manifest_dir, root)?,
            arch_specific: workspace_arch || arch_specific_manifest(&pkg.manifest_path),
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
            });
            edges.insert(Edge {
                from: pkg.name.clone(),
                to: dep.name.clone(),
            });
        }
    }
    Ok(())
}

fn unused_dependencies(metas: &[CargoMetadata], root: &Path) -> Result<Vec<String>> {
    let lib_names = lib_target_names(metas);
    let mut findings = BTreeSet::new();
    for meta in metas {
        for pkg in &meta.packages {
            let manifest_dir = pkg
                .manifest_path
                .parent()
                .with_context(|| format!("crate {}: manifest path has no parent", pkg.name))?;
            let paths: Vec<&MetaDependency> = pkg
                .dependencies
                .iter()
                .filter(|dep| dep.path.is_some())
                .collect();
            if paths.is_empty() {
                continue;
            }
            let sources = read_sources(manifest_dir)?;
            for dep in paths {
                // В коде зависимость зовут либо переименованием из манифеста, либо именем её
                // lib-цели: у пакета оно вправе отличаться от собственного.
                let referenced = dep.rename.as_deref().unwrap_or_else(|| {
                    lib_names
                        .get(dep.name.as_str())
                        .map_or(dep.name.as_str(), |name| name.as_str())
                });
                if !mentions_crate(&sources, referenced) {
                    findings.insert(format!(
                        "{}: dependency `{}` is declared but never used in {}",
                        pkg.name,
                        dep.name,
                        relative_dir(&pkg.name, manifest_dir, root)?
                    ));
                }
            }
        }
    }
    Ok(findings.into_iter().collect())
}

fn lib_target_names(metas: &[CargoMetadata]) -> BTreeMap<&str, &String> {
    let mut names = BTreeMap::new();
    for meta in metas {
        for pkg in &meta.packages {
            let lib = pkg
                .targets
                .iter()
                .find(|target| target.kind.iter().any(|kind| kind.contains("lib")));
            if let Some(lib) = lib {
                names.insert(pkg.name.as_str(), &lib.name);
            }
        }
    }
    names
}

fn read_sources(dir: &Path) -> Result<Vec<String>> {
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

fn mentions_crate(sources: &[String], dep: &str) -> bool {
    let ident = dep.replace('-', "_");
    sources.iter().any(|text| {
        text.match_indices(&ident).any(|(at, _)| {
            let before = text[..at].chars().next_back();
            let after = text[at + ident.len()..].chars().next();
            !before.is_some_and(is_ident_char) && !after.is_some_and(is_ident_char)
        })
    })
}

fn is_ident_char(ch: char) -> bool {
    ch.is_alphanumeric() || ch == '_'
}

fn merge_crates(crates: BTreeSet<WorkspaceCrate>) -> Vec<WorkspaceCrate> {
    // Пакет приходит с манифестом, цель path-зависимости - без него: признак
    // архитектуры собирается по всем вхождениям крейта.
    let mut merged: BTreeMap<(String, String), bool> = BTreeMap::new();
    for krate in crates {
        let known = merged
            .entry((krate.name, krate.dir))
            .or_insert(krate.arch_specific);
        *known |= krate.arch_specific;
    }
    merged
        .into_iter()
        .map(|((name, dir), arch_specific)| WorkspaceCrate {
            name,
            dir,
            arch_specific,
        })
        .collect()
}

fn arch_specific_manifest(manifest: &Path) -> bool {
    let Ok(text) = fs::read_to_string(manifest) else {
        return false;
    };
    let Ok(table) = text.parse::<toml::Table>() else {
        return false;
    };
    let Some(package) = table.get("package").and_then(toml::Value::as_table) else {
        return false;
    };
    let forced = package
        .get("forced-target")
        .and_then(toml::Value::as_str)
        .is_some_and(|target| target.split('-').any(|segment| segment == ARCH_SEGMENT));
    forced || declared_arch(package) == Some(ARCH_SEGMENT)
}

fn workspace_arch(meta: &CargoMetadata) -> Option<&str> {
    meta.workspace_metadata
        .as_ref()?
        .get("molecular")?
        .get("arch")?
        .as_str()
}

fn declared_arch(package: &toml::Table) -> Option<&str> {
    package
        .get("metadata")
        .and_then(toml::Value::as_table)?
        .get("molecular")
        .and_then(toml::Value::as_table)?
        .get("arch")
        .and_then(toml::Value::as_str)
}

fn relative_dir(name: &str, dir: &Path, root: &Path) -> Result<String> {
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

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Domain {
    Lib,
    Kernel,
    User,
    Tools,
    Xtask,
    Fuzz,
}

impl Domain {
    pub(crate) fn from_dir(dir: &str) -> Result<Self> {
        Ok(match dir {
            "lib" => Self::Lib,
            "kernel" => Self::Kernel,
            "user" => Self::User,
            "tools" => Self::Tools,
            "xtask" => Self::Xtask,
            "fuzz" => Self::Fuzz,
            other => bail!("unknown directory '{other}': layer domain is not defined"),
        })
    }

    fn name(self) -> &'static str {
        match self {
            Self::Lib => "lib",
            Self::Kernel => "kernel",
            Self::User => "user",
            Self::Tools => "tools",
            Self::Xtask => "xtask",
            Self::Fuzz => "fuzz",
        }
    }

    fn allows(self, dep: Self) -> bool {
        match self {
            Self::Lib => matches!(dep, Self::Lib),
            Self::Kernel => matches!(dep, Self::Lib | Self::Kernel),
            Self::User => matches!(dep, Self::Lib | Self::User),
            Self::Tools => matches!(dep, Self::Lib | Self::Tools),
            Self::Xtask => !matches!(dep, Self::Fuzz),
            Self::Fuzz => matches!(dep, Self::Lib | Self::Fuzz),
        }
    }
}

impl fmt::Display for Domain {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.name())
    }
}

#[derive(Debug, PartialEq, Eq)]
pub(crate) enum Violation {
    Domain {
        from: String,
        from_domain: Domain,
        to: String,
        to_domain: Domain,
    },
    Arch {
        from: String,
        to: String,
    },
}

impl fmt::Display for Violation {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Domain {
                from,
                from_domain,
                to,
                to_domain,
            } => write!(
                f,
                "crate {from} (domain {from_domain}) -> crate {to} (domain {to_domain})"
            ),
            Self::Arch { from, to } => write!(
                f,
                "crate {from} (arch-neutral) -> crate {to} ({ARCH_SEGMENT}-specific)"
            ),
        }
    }
}

#[derive(Clone, Copy)]
struct CrateRules<'a> {
    dir: &'a str,
    domain: Domain,
    arch_specific: bool,
}

impl<'a> CrateRules<'a> {
    fn from_dir(dir: &'a str) -> Result<Self> {
        let top_dir = dir.split('/').next().unwrap_or_default();
        Ok(Self {
            dir,
            domain: Domain::from_dir(top_dir)?,
            arch_specific: dir.split(['/', '-']).any(|segment| segment == ARCH_SEGMENT),
        })
    }
}

pub(crate) fn check_layers(crates: &[WorkspaceCrate], edges: &[Edge]) -> Result<Vec<Violation>> {
    let mut rules: BTreeMap<&str, CrateRules<'_>> = BTreeMap::new();
    for krate in crates {
        let mut crate_rules =
            CrateRules::from_dir(&krate.dir).with_context(|| format!("crate {}", krate.name))?;
        crate_rules.arch_specific |= krate.arch_specific;
        if let Some(known) = rules.insert(krate.name.as_str(), crate_rules) {
            if known.dir != crate_rules.dir {
                bail!(
                    "crate {}: conflicting directories '{}' and '{}'",
                    krate.name,
                    known.dir,
                    crate_rules.dir
                );
            }
            // Запись пакета несёт манифест, запись цели path-зависимости - нет:
            // признак архитектуры собирается по всем вхождениям крейта.
            let merged = CrateRules {
                arch_specific: known.arch_specific || crate_rules.arch_specific,
                ..crate_rules
            };
            rules.insert(krate.name.as_str(), merged);
        }
    }

    let mut violations = Vec::new();
    for edge in edges {
        let Some(&from_rules) = rules.get(edge.from.as_str()) else {
            bail!(
                "edge {} -> {}: crate {} is not in the crate list",
                edge.from,
                edge.to,
                edge.from
            );
        };
        let Some(&to_rules) = rules.get(edge.to.as_str()) else {
            bail!(
                "edge {} -> {}: crate {} is not in the crate list",
                edge.from,
                edge.to,
                edge.to
            );
        };
        if !from_rules.domain.allows(to_rules.domain) {
            violations.push(Violation::Domain {
                from: edge.from.clone(),
                from_domain: from_rules.domain,
                to: edge.to.clone(),
                to_domain: to_rules.domain,
            });
        }
        if !from_rules.arch_specific && to_rules.arch_specific {
            violations.push(Violation::Arch {
                from: edge.from.clone(),
                to: edge.to.clone(),
            });
        }
    }
    Ok(violations)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn crates() -> Vec<WorkspaceCrate> {
        [
            ("a-lib", "lib/a-lib"),
            ("b-lib", "lib/b-lib"),
            ("a-kernel", "kernel/a-kernel"),
            ("b-kernel", "kernel/b-kernel"),
            ("hal-arch", "kernel/hal-aarch64"),
            ("hal-arch-paging", "kernel/hal-aarch64-paging"),
            ("a-user", "user/a-user"),
            ("b-user", "user/b-user"),
            ("a-tools", "tools/a-tools"),
            ("b-tools", "tools/b-tools"),
            ("xtask", "xtask"),
            ("mobile-fuzz", "fuzz"),
        ]
        .map(|(name, dir)| WorkspaceCrate {
            name: name.to_string(),
            dir: dir.to_string(),
            arch_specific: false,
        })
        .to_vec()
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

    #[test]
    fn one_crate_in_two_directories_is_error() {
        let crates = vec![
            WorkspaceCrate {
                name: "a-lib".to_string(),
                dir: "lib/a-lib".to_string(),
                arch_specific: false,
            },
            WorkspaceCrate {
                name: "a-lib".to_string(),
                dir: "kernel/a-lib".to_string(),
                arch_specific: false,
            },
        ];
        assert!(check_layers(&crates, &[]).is_err());
    }

    #[test]
    fn allowed_edges_produce_no_violations() {
        let edges = edges(&[
            ("a-lib", "b-lib"),
            ("a-kernel", "a-lib"),
            ("a-kernel", "b-kernel"),
            ("a-user", "a-lib"),
            ("a-user", "b-user"),
            ("a-tools", "a-lib"),
            ("a-tools", "b-tools"),
            ("xtask", "a-lib"),
            ("xtask", "a-kernel"),
            ("xtask", "a-user"),
            ("xtask", "a-tools"),
            ("mobile-fuzz", "a-lib"),
        ]);
        assert!(check_layers(&crates(), &edges).unwrap().is_empty());
    }

    #[test]
    fn lib_depends_only_on_lib() {
        let edges = edges(&[
            ("a-lib", "a-kernel"),
            ("a-lib", "a-user"),
            ("a-lib", "a-tools"),
            ("a-lib", "xtask"),
            ("a-lib", "mobile-fuzz"),
        ]);
        assert_eq!(check_layers(&crates(), &edges).unwrap().len(), 5);
    }

    #[test]
    fn kernel_cannot_depend_on_user_tools_xtask_fuzz() {
        let edges = edges(&[
            ("a-kernel", "a-user"),
            ("a-kernel", "a-tools"),
            ("a-kernel", "xtask"),
            ("a-kernel", "mobile-fuzz"),
        ]);
        assert_eq!(check_layers(&crates(), &edges).unwrap().len(), 4);
    }

    #[test]
    fn user_cannot_depend_on_kernel_tools_xtask_fuzz() {
        let edges = edges(&[
            ("a-user", "a-kernel"),
            ("a-user", "a-tools"),
            ("a-user", "xtask"),
            ("a-user", "mobile-fuzz"),
        ]);
        assert_eq!(check_layers(&crates(), &edges).unwrap().len(), 4);
    }

    #[test]
    fn tools_cannot_depend_on_kernel_user_xtask_fuzz() {
        let edges = edges(&[
            ("a-tools", "a-kernel"),
            ("a-tools", "a-user"),
            ("a-tools", "xtask"),
            ("a-tools", "mobile-fuzz"),
        ]);
        assert_eq!(check_layers(&crates(), &edges).unwrap().len(), 4);
    }

    #[test]
    fn fuzz_cannot_depend_on_kernel_user_tools_xtask() {
        let edges = edges(&[
            ("mobile-fuzz", "a-kernel"),
            ("mobile-fuzz", "a-user"),
            ("mobile-fuzz", "a-tools"),
            ("mobile-fuzz", "xtask"),
        ]);
        assert_eq!(check_layers(&crates(), &edges).unwrap().len(), 4);
    }

    #[test]
    fn xtask_cannot_depend_on_fuzz() {
        let edges = edges(&[("xtask", "mobile-fuzz")]);
        assert_eq!(check_layers(&crates(), &edges).unwrap().len(), 1);
    }

    #[test]
    fn arch_neutral_crate_cannot_depend_on_arch_specific() {
        let edges = edges(&[
            ("a-kernel", "hal-arch"),
            ("a-kernel", "hal-arch-paging"),
            ("xtask", "hal-arch"),
        ]);
        assert_eq!(check_layers(&crates(), &edges).unwrap().len(), 3);
    }

    #[test]
    fn arch_specific_crate_depends_on_arch_and_neutral() {
        let edges = edges(&[
            ("hal-arch", "hal-arch-paging"),
            ("hal-arch", "a-kernel"),
            ("hal-arch", "a-lib"),
        ]);
        assert!(check_layers(&crates(), &edges).unwrap().is_empty());
    }

    #[test]
    fn domain_violation_lists_crates_and_domains() {
        let edges = edges(&[("a-user", "a-kernel")]);
        let violations = check_layers(&crates(), &edges).unwrap();
        assert_eq!(
            violations[0].to_string(),
            "crate a-user (domain user) -> crate a-kernel (domain kernel)"
        );
    }

    #[test]
    fn arch_violation_lists_crates_and_arch() {
        let edges = edges(&[("a-kernel", "hal-arch")]);
        let violations = check_layers(&crates(), &edges).unwrap();
        assert_eq!(
            violations[0].to_string(),
            "crate a-kernel (arch-neutral) -> crate hal-arch (aarch64-specific)"
        );
    }

    #[test]
    fn domain_and_arch_violations_are_reported_together() {
        let edges = edges(&[("a-user", "hal-arch")]);
        assert_eq!(check_layers(&crates(), &edges).unwrap().len(), 2);
    }

    #[test]
    fn a_forced_target_crate_is_arch_specific_whatever_its_path() {
        let mut crates = crates();
        crates.push(WorkspaceCrate {
            name: "arch-user".to_string(),
            dir: "user/arch-user".to_string(),
            arch_specific: true,
        });
        crates.push(WorkspaceCrate {
            name: "arch-lib".to_string(),
            dir: "lib/arch-lib".to_string(),
            arch_specific: true,
        });

        // Крейт, собираемый только под архитектуру, вправе зависеть от
        // архитектурного, а нейтральный - нет.
        let allowed = edges(&[("arch-user", "arch-lib")]);
        assert!(check_layers(&crates, &allowed).unwrap().is_empty());

        let refused = edges(&[("a-user", "arch-lib"), ("a-lib", "arch-lib")]);
        assert_eq!(check_layers(&crates, &refused).unwrap().len(), 2);
    }

    #[test]
    fn unknown_directory_is_error() {
        let crates = vec![WorkspaceCrate {
            name: "stray".to_string(),
            dir: "scripts/stray".to_string(),
            arch_specific: false,
        }];
        assert!(check_layers(&crates, &[]).is_err());
    }

    #[test]
    fn edge_with_unknown_crate_is_error() {
        let edges = edges(&[("a-lib", "ghost")]);
        assert!(check_layers(&crates(), &edges).is_err());
    }

    #[test]
    fn crate_identifier_is_matched_as_a_whole_word() {
        let sources = vec!["use ipc_test::MockEnd;\nlet x = my_ipc + 1;\n".to_string()];
        assert!(mentions_crate(&sources, "ipc-test"));
        assert!(!mentions_crate(&sources, "ipc"));
        assert!(!mentions_crate(&sources, "test"));
    }

    #[test]
    fn lib_target_name_wins_over_the_package_name() {
        let meta = CargoMetadata {
            packages: vec![MetaPackage {
                name: "test-harness-qemu".to_string(),
                manifest_path: PathBuf::from("lib/test-harness-qemu/Cargo.toml"),
                dependencies: Vec::new(),
                targets: vec![MetaTarget {
                    name: "kernel_tests".to_string(),
                    kind: vec!["lib".to_string()],
                }],
            }],
            workspace_metadata: None,
        };
        let names = lib_target_names(std::slice::from_ref(&meta));
        assert_eq!(
            names.get("test-harness-qemu").map(|name| name.as_str()),
            Some("kernel_tests")
        );
    }
}
