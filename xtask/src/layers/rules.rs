//! Таблица разрешённых рёбер: домен крейта задаётся его верхней директорией, а
//! архитектурно-зависимый крейт доступен только другому архитектурно-зависимому.

use std::{
    collections::{BTreeMap, btree_map::Entry},
    fmt,
};

use anyhow::{Context, Result, bail};

use crate::layers::{
    ARCH_SEGMENT,
    metadata::{Edge, WorkspaceCrate},
};

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) enum Domain {
    Lib,
    Kernel,
    User,
    Tools,
    Xtask,
    Fuzz,
}

impl Domain {
    fn from_dir(dir: &str) -> Result<Self> {
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
pub(super) enum Violation {
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

pub(super) fn check_layers(crates: &[WorkspaceCrate], edges: &[Edge]) -> Result<Vec<Violation>> {
    let mut rules: BTreeMap<&str, CrateRules<'_>> = BTreeMap::new();
    for krate in crates {
        let mut crate_rules =
            CrateRules::from_dir(&krate.dir).with_context(|| format!("crate {}", krate.name))?;
        crate_rules.arch_specific |= krate.arch_specific;
        match rules.entry(krate.name.as_str()) {
            Entry::Vacant(slot) => {
                slot.insert(crate_rules);
            }
            Entry::Occupied(mut slot) => {
                if slot.get().dir != crate_rules.dir {
                    bail!(
                        "crate {}: conflicting directories '{}' and '{}'",
                        krate.name,
                        slot.get().dir,
                        crate_rules.dir
                    );
                }
                slot.get_mut().arch_specific |= crate_rules.arch_specific;
            }
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
}
