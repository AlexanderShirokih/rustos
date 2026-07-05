//! Проверка правил слоев workspace: домен крейта задается его верхней
//! директорией, ребра зависимостей сверяются с таблицей разрешений.

use std::{
    collections::{BTreeMap, BTreeSet},
    fmt,
    path::PathBuf,
    process::Command,
};

use anyhow::{Context, Result, bail};
use serde::Deserialize;

use crate::context::project_root;

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

/// Собирает крейты и ребра workspace из `cargo metadata` и прогоняет их
/// через [`check_layers`]; нарушения печатает и завершается ошибкой.
pub(crate) fn run() -> Result<()> {
    let output = Command::new("cargo")
        .args(["metadata", "--format-version", "1", "--no-deps"])
        .current_dir(project_root())
        .output()
        .context("Failed to run cargo metadata")?;
    if !output.status.success() {
        bail!("cargo metadata failed with {}", output.status);
    }
    let meta: CargoMetadata =
        serde_json::from_slice(&output.stdout).context("Failed to parse cargo metadata")?;

    let crates = meta
        .packages
        .iter()
        .map(|pkg| {
            let rel = pkg
                .manifest_path
                .strip_prefix(&meta.workspace_root)
                .with_context(|| format!("crate {} is outside the workspace root", pkg.name))?;
            let dir = rel
                .components()
                .next()
                .and_then(|c| c.as_os_str().to_str())
                .with_context(|| format!("crate {}: empty relative path", pkg.name))?;
            Ok((pkg.name.clone(), dir.to_string()))
        })
        .collect::<Result<Vec<_>>>()?;

    let members: BTreeSet<&str> = crates.iter().map(|(name, _)| name.as_str()).collect();
    // dependencies включает normal-, dev- и build-зависимости; path = Some
    // отбирает локальные, дубликаты (normal + dev) схлопываются через BTreeSet.
    let edges: Vec<(String, String)> = meta
        .packages
        .iter()
        .flat_map(|pkg| {
            pkg.dependencies
                .iter()
                .filter(|dep| dep.path.is_some() && members.contains(dep.name.as_str()))
                .map(|dep| (pkg.name.clone(), dep.name.clone()))
        })
        .collect::<BTreeSet<_>>()
        .into_iter()
        .collect();

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

/// Домен крейта: верхняя директория относительно корня workspace.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Domain {
    Lib,
    Kernel,
    User,
    Tools,
    Xtask,
}

impl Domain {
    /// Определяет домен по имени верхней директории; незнакомая директория - ошибка.
    pub fn from_dir(dir: &str) -> Result<Self> {
        Ok(match dir {
            "lib" => Self::Lib,
            "kernel" => Self::Kernel,
            "user" => Self::User,
            "tools" => Self::Tools,
            "xtask" => Self::Xtask,
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
        }
    }

    /// Разрешено ли крейту этого домена зависеть от крейта домена `dep`.
    fn allows(self, dep: Self) -> bool {
        match self {
            Self::Lib => matches!(dep, Self::Lib),
            Self::Kernel => matches!(dep, Self::Lib | Self::Kernel),
            Self::User => matches!(dep, Self::Lib | Self::User),
            Self::Tools => matches!(dep, Self::Lib | Self::Tools),
            Self::Xtask => true,
        }
    }
}

impl fmt::Display for Domain {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.name())
    }
}

/// Запрещенное ребро зависимости между крейтами разных доменов.
#[derive(Debug, PartialEq, Eq)]
pub struct Violation {
    pub from: String,
    pub from_domain: Domain,
    pub to: String,
    pub to_domain: Domain,
}

impl fmt::Display for Violation {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(
            f,
            "crate {} (domain {}) -> crate {} (domain {})",
            self.from, self.from_domain, self.to, self.to_domain
        )
    }
}

/// Проверяет ребра зависимостей по правилам слоев.
/// `crates` - пары (имя крейта, верхняя директория), `edges` - пары (от, к).
pub fn check_layers(
    crates: &[(String, String)],
    edges: &[(String, String)],
) -> Result<Vec<Violation>> {
    let mut domains = BTreeMap::new();
    for (name, dir) in crates {
        let domain = Domain::from_dir(dir).with_context(|| format!("crate {name}"))?;
        domains.insert(name.as_str(), domain);
    }

    let mut violations = Vec::new();
    for (from, to) in edges {
        let Some(&from_domain) = domains.get(from.as_str()) else {
            bail!("edge {from} -> {to}: crate {from} is not in the crate list");
        };
        let Some(&to_domain) = domains.get(to.as_str()) else {
            bail!("edge {from} -> {to}: crate {to} is not in the crate list");
        };
        if !from_domain.allows(to_domain) {
            violations.push(Violation {
                from: from.clone(),
                from_domain,
                to: to.clone(),
                to_domain,
            });
        }
    }
    Ok(violations)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn crates() -> Vec<(String, String)> {
        [
            ("a-lib", "lib"),
            ("b-lib", "lib"),
            ("a-kernel", "kernel"),
            ("b-kernel", "kernel"),
            ("a-user", "user"),
            ("b-user", "user"),
            ("a-tools", "tools"),
            ("b-tools", "tools"),
            ("xtask", "xtask"),
        ]
        .map(|(n, d)| (n.to_string(), d.to_string()))
        .to_vec()
    }

    fn edges(pairs: &[(&str, &str)]) -> Vec<(String, String)> {
        pairs
            .iter()
            .map(|(a, b)| ((*a).to_string(), (*b).to_string()))
            .collect()
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
        ]);
        assert_eq!(check_layers(&crates(), &edges).unwrap().len(), 4);
    }

    #[test]
    fn kernel_cannot_depend_on_user_tools_xtask() {
        let edges = edges(&[
            ("a-kernel", "a-user"),
            ("a-kernel", "a-tools"),
            ("a-kernel", "xtask"),
        ]);
        assert_eq!(check_layers(&crates(), &edges).unwrap().len(), 3);
    }

    #[test]
    fn user_cannot_depend_on_kernel_tools_xtask() {
        let edges = edges(&[
            ("a-user", "a-kernel"),
            ("a-user", "a-tools"),
            ("a-user", "xtask"),
        ]);
        assert_eq!(check_layers(&crates(), &edges).unwrap().len(), 3);
    }

    #[test]
    fn tools_cannot_depend_on_kernel_user_xtask() {
        let edges = edges(&[
            ("a-tools", "a-kernel"),
            ("a-tools", "a-user"),
            ("a-tools", "xtask"),
        ]);
        assert_eq!(check_layers(&crates(), &edges).unwrap().len(), 3);
    }

    #[test]
    fn violation_lists_crates_and_domains() {
        let edges = edges(&[("a-user", "a-kernel")]);
        let violations = check_layers(&crates(), &edges).unwrap();
        assert_eq!(
            violations[0].to_string(),
            "crate a-user (domain user) -> crate a-kernel (domain kernel)"
        );
    }

    #[test]
    fn unknown_directory_is_error() {
        let crates = vec![("stray".to_string(), "scripts".to_string())];
        assert!(check_layers(&crates, &[]).is_err());
    }

    #[test]
    fn edge_with_unknown_crate_is_error() {
        let edges = edges(&[("a-lib", "ghost")]);
        assert!(check_layers(&crates(), &edges).is_err());
    }
}
