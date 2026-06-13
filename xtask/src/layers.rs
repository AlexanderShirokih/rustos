//! Проверка правил слоев workspace: домен крейта задается его верхней
//! директорией, ребра зависимостей сверяются с таблицей разрешений.

use std::{collections::BTreeMap, fmt};

use anyhow::{Context, Result, bail};

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
            other => bail!("неизвестная директория '{other}': домен слоя не определен"),
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
            "крейт {} (домен {}) -> крейт {} (домен {})",
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
        let domain = Domain::from_dir(dir).with_context(|| format!("крейт {name}"))?;
        domains.insert(name.as_str(), domain);
    }

    let mut violations = Vec::new();
    for (from, to) in edges {
        let Some(&from_domain) = domains.get(from.as_str()) else {
            bail!("ребро {from} -> {to}: крейт {from} отсутствует в списке крейтов");
        };
        let Some(&to_domain) = domains.get(to.as_str()) else {
            bail!("ребро {from} -> {to}: крейт {to} отсутствует в списке крейтов");
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
            "крейт a-user (домен user) -> крейт a-kernel (домен kernel)"
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
