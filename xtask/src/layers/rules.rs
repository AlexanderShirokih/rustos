//! Таблица разрешённых рёбер: домен крейта задаётся его верхней директорией, а
//! архитектурно-зависимый крейт доступен только крейту той же архитектуры.

use std::{
    collections::{BTreeMap, BTreeSet, btree_map::Entry},
    fmt,
};

use anyhow::{Context, Result, bail};

use crate::layers::metadata::{Edge, WorkspaceCrate};

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
        from_arch: Option<String>,
        to: String,
        to_arch: String,
    },
    ProcessState {
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
            Self::Arch {
                from,
                from_arch,
                to,
                to_arch,
            } => {
                let from_arch = from_arch.as_ref().map_or_else(
                    || "arch-neutral".to_string(),
                    |arch| format!("{arch}-specific"),
                );
                write!(
                    f,
                    "crate {from} ({from_arch}) -> crate {to} ({to_arch}-specific)"
                )
            }
            Self::ProcessState { from, to } => write!(
                f,
                "crate {from} (linked with std) -> crate {to} (owns the process state)"
            ),
        }
    }
}

#[derive(Clone, Copy)]
struct CrateRules<'a> {
    dir: &'a str,
    domain: Domain,
    arch: Option<&'a str>,
    owns_process_state: bool,
    links_std: bool,
}

impl<'a> CrateRules<'a> {
    fn of(krate: &'a WorkspaceCrate) -> Result<Self> {
        let top_dir = krate.dir.split('/').next().unwrap_or_default();
        Ok(Self {
            dir: &krate.dir,
            domain: Domain::from_dir(top_dir)?,
            arch: krate.arch.as_deref(),
            owns_process_state: krate.owns_process_state,
            links_std: krate.links_std,
        })
    }
}

pub(super) fn check_layers(crates: &[WorkspaceCrate], edges: &[Edge]) -> Result<Vec<Violation>> {
    let mut rules: BTreeMap<&str, CrateRules<'_>> = BTreeMap::new();
    for krate in crates {
        let crate_rules = CrateRules::of(krate).with_context(|| format!("crate {}", krate.name))?;
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
                if crate_rules.arch.is_some() {
                    slot.get_mut().arch = crate_rules.arch;
                }
                slot.get_mut().owns_process_state |= crate_rules.owns_process_state;
                slot.get_mut().links_std |= crate_rules.links_std;
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
        // Зависимость допустима, когда она нейтральна или её архитектура совпадает.
        if let Some(to_arch) = to_rules.arch
            && from_rules.arch != Some(to_arch)
        {
            violations.push(Violation::Arch {
                from: edge.from.clone(),
                from_arch: from_rules.arch.map(str::to_owned),
                to: edge.to.clone(),
                to_arch: to_arch.to_owned(),
            });
        }
    }
    violations.extend(process_state_violations(&rules, edges));
    Ok(violations)
}

fn process_state_violations(
    rules: &BTreeMap<&str, CrateRules<'_>>,
    edges: &[Edge],
) -> Vec<Violation> {
    let mut outgoing: BTreeMap<&str, Vec<&str>> = BTreeMap::new();
    for edge in edges {
        outgoing
            .entry(edge.from.as_str())
            .or_default()
            .push(edge.to.as_str());
    }

    // Состояние процесса у крейта со стандартной библиотекой уже есть - её собственное, и
    // вторая копия ломает механизмы, которые это состояние держат. Копия приходит и через
    // промежуточный крейт, поэтому владельца ищет обход графа.
    let mut violations = Vec::new();
    for (&name, crate_rules) in rules {
        if !crate_rules.links_std {
            continue;
        }
        let mut seen: BTreeSet<&str> = BTreeSet::new();
        let mut pending = vec![name];
        while let Some(current) = pending.pop() {
            for &next in outgoing.get(current).into_iter().flatten() {
                if !seen.insert(next) {
                    continue;
                }
                if rules
                    .get(next)
                    .is_some_and(|rules| rules.owns_process_state)
                {
                    violations.push(Violation::ProcessState {
                        from: name.to_string(),
                        to: next.to_string(),
                    });
                    continue;
                }
                pending.push(next);
            }
        }
    }
    violations
}

#[cfg(test)]
mod tests {
    use super::*;

    fn plain(name: &str, dir: &str) -> WorkspaceCrate {
        WorkspaceCrate {
            name: name.to_string(),
            dir: dir.to_string(),
            arch: None,
            owns_process_state: false,
            links_std: false,
        }
    }

    fn arch(name: &str, dir: &str, arch: &str) -> WorkspaceCrate {
        WorkspaceCrate {
            arch: Some(arch.to_string()),
            ..plain(name, dir)
        }
    }

    fn crates() -> Vec<WorkspaceCrate> {
        let mut crates = [
            ("a-lib", "lib/a-lib"),
            ("b-lib", "lib/b-lib"),
            ("a-kernel", "kernel/a-kernel"),
            ("b-kernel", "kernel/b-kernel"),
            ("a-user", "user/a-user"),
            ("b-user", "user/b-user"),
            ("a-tools", "tools/a-tools"),
            ("b-tools", "tools/b-tools"),
            ("xtask", "xtask"),
            ("mobile-fuzz", "fuzz"),
        ]
        .map(|(name, dir)| plain(name, dir))
        .to_vec();
        crates.push(arch("hal-arch", "kernel/hal-aarch64", "aarch64"));
        crates.push(arch(
            "hal-arch-paging",
            "kernel/hal-aarch64-paging",
            "aarch64",
        ));
        crates.push(arch("hal-other", "kernel/hal-riscv64", "riscv64"));
        crates
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
        let crates = vec![plain("a-lib", "lib/a-lib"), plain("a-lib", "kernel/a-lib")];
        assert!(check_layers(&crates, &[]).is_err());
    }

    fn platform_crates() -> Vec<WorkspaceCrate> {
        let mut crates = crates();
        crates.push(WorkspaceCrate {
            owns_process_state: true,
            ..plain("platform", "lib/platform")
        });
        crates.push(WorkspaceCrate {
            links_std: true,
            ..plain("std-user", "user/std-user")
        });
        crates
    }

    #[test]
    fn crate_with_std_takes_no_second_owner_of_the_process_state() {
        let violations =
            check_layers(&platform_crates(), &edges(&[("std-user", "platform")])).unwrap();
        assert_eq!(
            violations,
            vec![Violation::ProcessState {
                from: "std-user".to_string(),
                to: "platform".to_string(),
            }]
        );
    }

    #[test]
    fn the_process_state_owner_is_found_through_an_intermediate_crate() {
        let violations = check_layers(
            &platform_crates(),
            &edges(&[("std-user", "a-lib"), ("a-lib", "platform")]),
        )
        .unwrap();
        assert_eq!(
            violations,
            vec![Violation::ProcessState {
                from: "std-user".to_string(),
                to: "platform".to_string(),
            }]
        );
    }

    #[test]
    fn crate_without_std_takes_the_process_state_layer() {
        let violations =
            check_layers(&platform_crates(), &edges(&[("a-user", "platform")])).unwrap();
        assert!(violations.is_empty());
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
    fn a_crate_of_one_arch_cannot_depend_on_a_crate_of_another() {
        let edges = edges(&[("hal-other", "hal-arch-paging")]);
        let violations = check_layers(&crates(), &edges).unwrap();
        assert_eq!(
            violations,
            vec![Violation::Arch {
                from: "hal-other".to_string(),
                from_arch: Some("riscv64".to_string()),
                to: "hal-arch-paging".to_string(),
                to_arch: "aarch64".to_string(),
            }]
        );
        assert_eq!(
            violations[0].to_string(),
            "crate hal-other (riscv64-specific) -> crate hal-arch-paging (aarch64-specific)"
        );
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
        crates.push(arch("arch-user", "user/arch-user", "aarch64"));
        crates.push(arch("arch-lib", "lib/arch-lib", "aarch64"));

        // Крейт, собираемый только под архитектуру, вправе зависеть от
        // архитектурного, а нейтральный - нет.
        let allowed = edges(&[("arch-user", "arch-lib")]);
        assert!(check_layers(&crates, &allowed).unwrap().is_empty());

        let refused = edges(&[("a-user", "arch-lib"), ("a-lib", "arch-lib")]);
        assert_eq!(check_layers(&crates, &refused).unwrap().len(), 2);
    }

    #[test]
    fn unknown_directory_is_error() {
        let crates = vec![plain("stray", "scripts/stray")];
        assert!(check_layers(&crates, &[]).is_err());
    }

    #[test]
    fn edge_with_unknown_crate_is_error() {
        let edges = edges(&[("a-lib", "ghost")]);
        assert!(check_layers(&crates(), &edges).is_err());
    }
}
