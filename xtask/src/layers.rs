//! Проверка правил слоев workspace: домен крейта задается его верхней директорией, ребра
//! зависимостей сверяются с таблицей разрешений. Архитектурно-зависимые крейты (сегмент
//! `aarch64` в пути) доступны только другим архитектурно-зависимым крейтам.

use std::{
    collections::{BTreeMap, BTreeSet},
    fmt,
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
    workspace_root: PathBuf,
}

#[derive(Deserialize)]
struct MetaPackage {
    name: String,
    manifest_path: PathBuf,
    dependencies: Vec<MetaDependency>,
}

#[derive(Deserialize)]
struct MetaDependency {
    name: String,
    path: Option<PathBuf>,
}

/// Крейт workspace: имя пакета и директория относительно корня репозитория.
#[derive(Clone, Debug, PartialEq, Eq, PartialOrd, Ord)]
pub(crate) struct WorkspaceCrate {
    pub(crate) name: String,
    pub(crate) dir: String,
}

/// Ребро зависимости между крейтами workspace.
#[derive(Clone, Debug, PartialEq, Eq, PartialOrd, Ord)]
pub(crate) struct Edge {
    pub(crate) from: String,
    pub(crate) to: String,
}

/// Собирает крейты и ребра основного и fuzz-workspace из `cargo metadata` и прогоняет их
/// через [`check_layers`]. Нарушения печатает и завершается ошибкой.
pub(crate) fn run() -> Result<()> {
    let mut crates = BTreeSet::new();
    let mut edges = BTreeSet::new();

    let root_meta = load_metadata(&project_root())?;
    collect_workspace(
        &root_meta,
        &root_meta.workspace_root.clone(),
        &mut crates,
        &mut edges,
    )?;

    let fuzz_meta = load_metadata(&project_root().join("fuzz"))?;
    let fuzz_root = fuzz_meta
        .workspace_root
        .parent()
        .context("fuzz workspace root has no parent directory")?
        .to_path_buf();
    collect_workspace(&fuzz_meta, &fuzz_root, &mut crates, &mut edges)?;

    let crates: Vec<WorkspaceCrate> = crates.into_iter().collect();
    let edges: Vec<Edge> = edges.into_iter().collect();

    let violations = check_layers(&crates, &edges)?;
    if !violations.is_empty() {
        for violation in &violations {
            eprintln!("{violation}");
        }
        bail!("check-layers: {} layer rule violations", violations.len());
    }
    println!(
        "check-layers: OK ({} crates, {} edges)",
        crates.len(),
        edges.len()
    );
    Ok(())
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
    for pkg in &meta.packages {
        let manifest_dir = pkg
            .manifest_path
            .parent()
            .with_context(|| format!("crate {}: manifest path has no parent", pkg.name))?;
        crates.insert(WorkspaceCrate {
            name: pkg.name.clone(),
            dir: relative_dir(&pkg.name, manifest_dir, root)?,
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
            });
            edges.insert(Edge {
                from: pkg.name.clone(),
                to: dep.name.clone(),
            });
        }
    }
    Ok(())
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

/// Домен крейта: верхняя директория относительно корня репозитория.
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
    /// Определяет домен по имени верхней директории. Незнакомая директория - ошибка.
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

    /// Разрешено ли крейту этого домена зависеть от крейта домена `dep`.
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

/// Запрещенное ребро зависимости между крейтами.
#[derive(Debug, PartialEq, Eq)]
pub(crate) enum Violation {
    /// Нарушение таблицы разрешений между доменами.
    Domain {
        from: String,
        from_domain: Domain,
        to: String,
        to_domain: Domain,
    },
    /// Зависимость архитектурно-независимого крейта от архитектурного.
    Arch { from: String, to: String },
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

/// Проверяет ребра зависимостей по правилам слоев: таблица разрешений доменов и запрет
/// зависимости архитектурно-независимого крейта от архитектурного.
pub(crate) fn check_layers(crates: &[WorkspaceCrate], edges: &[Edge]) -> Result<Vec<Violation>> {
    let mut rules = BTreeMap::new();
    for krate in crates {
        let crate_rules =
            CrateRules::from_dir(&krate.dir).with_context(|| format!("crate {}", krate.name))?;
        if let Some(known) = rules.insert(krate.name.as_str(), crate_rules)
            && known.dir != crate_rules.dir
        {
            bail!(
                "crate {}: conflicting directories '{}' and '{}'",
                krate.name,
                known.dir,
                crate_rules.dir
            );
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

    fn metadata(root: &str, packages: &[(&str, &str, &[(&str, &str)])]) -> CargoMetadata {
        CargoMetadata {
            workspace_root: PathBuf::from(root),
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
                        })
                        .collect(),
                })
                .collect(),
        }
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
        let meta = metadata(
            "/repo",
            &[(
                "a-kernel",
                "/repo/kernel/a-kernel",
                &[("a-lib", "/repo/lib/a-lib")],
            )],
        );
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
        let meta = metadata(
            "/repo",
            &[(
                "a-kernel",
                "/repo/kernel/a-kernel",
                &[("vendored", "/vendor/vendored")],
            )],
        );
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
            },
            WorkspaceCrate {
                name: "a-lib".to_string(),
                dir: "kernel/a-lib".to_string(),
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
    fn unknown_directory_is_error() {
        let crates = vec![WorkspaceCrate {
            name: "stray".to_string(),
            dir: "scripts/stray".to_string(),
        }];
        assert!(check_layers(&crates, &[]).is_err());
    }

    #[test]
    fn edge_with_unknown_crate_is_error() {
        let edges = edges(&[("a-lib", "ghost")]);
        assert!(check_layers(&crates(), &edges).is_err());
    }
}
