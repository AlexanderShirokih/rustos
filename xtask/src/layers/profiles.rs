//! Профиль сборки workspace - таргет, доступность стандартной библиотеки и toolchain его
//! членов. Профиль объявлен ключом `workspace.metadata.molecular.profile`, и члены workspace
//! обязаны ему отвечать. Таблица профилей - в `docs/workspace.md`.

use std::{fmt, path::Path};

use anyhow::{Context, Result};

use crate::layers::metadata::{ArchEntry, Member};

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Profile {
    Common,
    Host,
    Platform,
    StdRuntime,
    Userland,
    Fuzz,
}

impl Profile {
    pub(crate) const ALL: [Self; 6] = [
        Self::Common,
        Self::Host,
        Self::Platform,
        Self::StdRuntime,
        Self::Userland,
        Self::Fuzz,
    ];

    pub(crate) fn name(self) -> &'static str {
        match self {
            Self::Common => "common",
            Self::Host => "host",
            Self::Platform => "platform",
            Self::StdRuntime => "std-runtime",
            Self::Userland => "userland",
            Self::Fuzz => "fuzz",
        }
    }

    pub(crate) fn parse(name: &str) -> Result<Self> {
        Self::ALL
            .into_iter()
            .find(|profile| profile.name() == name)
            .with_context(|| {
                let known: Vec<&str> = Self::ALL.iter().map(|profile| profile.name()).collect();
                format!(
                    "unknown profile `{name}`; known profiles: {}",
                    known.join(", ")
                )
            })
    }

    pub(crate) fn of_manifest(manifest: &toml::Table, display: &Path) -> Result<Self> {
        let name = manifest
            .get("workspace")
            .and_then(toml::Value::as_table)
            .and_then(|workspace| workspace.get("metadata"))
            .and_then(toml::Value::as_table)
            .and_then(|metadata| metadata.get("molecular"))
            .and_then(toml::Value::as_table)
            .and_then(|molecular| molecular.get("profile"))
            .and_then(toml::Value::as_str)
            .with_context(|| {
                format!(
                    "{} declares no workspace.metadata.molecular.profile",
                    display.display()
                )
            })?;
        Self::parse(name).with_context(|| display.display().to_string())
    }
}

impl fmt::Display for Profile {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.name())
    }
}

pub(super) fn check_members(
    profile: Profile,
    workspace: &Path,
    members: &[Member],
    registry: &[ArchEntry],
) -> Vec<String> {
    members
        .iter()
        .filter_map(|member| {
            member_fault(profile, member, registry).map(|fault| {
                format!(
                    "crate {} (workspace {}, profile {profile}): {fault}",
                    member.name,
                    workspace.display()
                )
            })
        })
        .collect()
}

fn member_fault(profile: Profile, member: &Member, registry: &[ArchEntry]) -> Option<String> {
    let forced = member.forced_target.as_deref();
    match profile {
        // Proc-macro собирается под хост при любом таргете и стандартную библиотеку линкует
        // по своей природе.
        Profile::Common | Profile::StdRuntime => {
            if let Some(target) = forced {
                return Some(format!("forced-target `{target}` is not allowed"));
            }
            (member.links_std && !member.proc_macro).then(|| "links the standard library".into())
        }
        Profile::Host | Profile::Fuzz => {
            if let Some(target) = forced {
                return Some(format!("forced-target `{target}` is not allowed"));
            }
            (!member.links_std).then(|| "does not link the standard library".into())
        }
        Profile::Platform => {
            let Some(target) = forced else {
                return Some("declares no forced-target".into());
            };
            (!registry.iter().any(|entry| entry.bare == target)).then(|| {
                format!(
                    "forced-target `{target}` is not the bare target of a registered architecture"
                )
            })
        }
        Profile::Userland => {
            forced.map(|target| format!("forced-target `{target}` is not allowed"))
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn registry() -> Vec<ArchEntry> {
        vec![ArchEntry {
            name: "aarch64".to_string(),
            bare: "aarch64-unknown-none".to_string(),
            system: "targets/aarch64-unknown-molecular.json".to_string(),
        }]
    }

    fn member(name: &str, links_std: bool, forced_target: Option<&str>) -> Member {
        Member {
            name: name.to_string(),
            links_std,
            proc_macro: false,
            forced_target: forced_target.map(str::to_owned),
        }
    }

    fn faults(profile: Profile, members: &[Member]) -> Vec<String> {
        check_members(profile, Path::new("/repo/ws"), members, &registry())
    }

    #[test]
    fn a_manifest_with_a_profile_outside_the_table_is_refused() {
        let manifest = "[workspace.metadata.molecular]\nprofile = \"bare\"\n"
            .parse()
            .unwrap();
        let error = Profile::of_manifest(&manifest, Path::new("Cargo.toml")).unwrap_err();
        assert!(
            format!("{error:#}").contains("unknown profile `bare`"),
            "{error:#}"
        );
    }

    #[test]
    fn a_common_workspace_refuses_std_and_forced_target() {
        let members = [
            member("a-lib", false, None),
            member("a-tool", true, None),
            member("hal", false, Some("aarch64-unknown-none")),
        ];
        let faults = faults(Profile::Common, &members);
        assert_eq!(faults.len(), 2);
        assert!(faults[0].starts_with("crate a-tool (workspace /repo/ws, profile common):"));
        assert!(faults[1].contains("forced-target `aarch64-unknown-none`"));
    }

    #[test]
    fn a_proc_macro_fits_the_common_profile() {
        let members = [Member {
            proc_macro: true,
            ..member("macros", true, None)
        }];
        assert!(faults(Profile::Common, &members).is_empty());
    }

    #[test]
    fn a_host_workspace_refuses_a_no_std_crate_and_forced_target() {
        let members = [
            member("a-tool", true, None),
            member("a-lib", false, None),
            member("hal", true, Some("aarch64-unknown-none")),
        ];
        let faults = faults(Profile::Host, &members);
        assert_eq!(faults.len(), 2);
        assert!(faults[0].contains("crate a-lib"));
        assert!(faults[1].contains("crate hal"));
    }

    #[test]
    fn a_platform_workspace_requires_a_registered_forced_target() {
        let members = [
            member("hal", false, Some("aarch64-unknown-none")),
            member("a-lib", false, None),
            member("hal-other", false, Some("riscv64gc-unknown-none-elf")),
        ];
        let faults = faults(Profile::Platform, &members);
        assert_eq!(faults.len(), 2);
        assert!(faults[0].contains("declares no forced-target"));
        assert!(faults[1].contains("riscv64gc-unknown-none-elf"));
    }

    #[test]
    fn a_userland_workspace_refuses_forced_target_only() {
        let members = [
            member("hello-std", true, None),
            member("rootkeeper", false, None),
            member("hal", false, Some("aarch64-unknown-none")),
        ];
        let faults = faults(Profile::Userland, &members);
        assert_eq!(faults.len(), 1);
        assert!(faults[0].contains("crate hal"));
    }
}
