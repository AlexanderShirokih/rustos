//! Clippy, rustdoc и хостовые тесты по профилю workspace: обход реестра workspace из
//! `workspace.exclude` корневого манифеста и запуск cargo в каждом по его профилю и по
//! записям реестра архитектур, по образцу `cargo xtask fmt`.
//!
//! Таблица "профиль -> команды" определяет профиль и меняется вместе с формой проверок.

use std::{
    path::{Path, PathBuf},
    process::Command,
};

use anyhow::{Result, bail};
use clap::Args;

use crate::{
    context::project_root,
    layers::{ArchEntry, Profile, Workspace, arch_registry, registered, workspaces},
    race, shell, userland,
};

#[derive(Args)]
pub(crate) struct ClippyArgs {
    /// Lint the host side only: crates built for the host
    #[arg(long)]
    host: bool,
    /// Lint the target side only: crates built for every registered architecture
    #[arg(long)]
    target: bool,
    /// Lint the target side for the named architecture only; repeat for several
    #[arg(long = "arch", value_name = "NAME")]
    arch: Vec<String>,
}

const RUSTDOC_FLAGS: &str = "-D rustdoc::broken_intra_doc_links -D warnings";

#[derive(Debug, PartialEq, Eq)]
pub(crate) struct Invocation {
    args: Vec<String>,
    dir: PathBuf,
    std_src: bool,
    rustdoc_flags: bool,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Side<'a> {
    Host,
    Arch(&'a ArchEntry),
}

pub(crate) fn clippy(args: &ClippyArgs) -> Result<()> {
    let root = project_root();
    let registry = arch_registry(&root)?;
    let sides = selected_sides(args, &registry)?;

    let mut invocations = Vec::new();
    for workspace in workspaces(&root)? {
        for side in &sides {
            invocations.extend(clippy_commands(&workspace, *side, &root));
        }
    }
    run_all(&invocations, "clippy")
}

pub(crate) fn test() -> Result<()> {
    let root = project_root();
    let host = race::host_target()?;
    let mut invocations = Vec::new();
    for workspace in workspaces(&root)? {
        invocations.extend(test_commands(&workspace, &root, &host));
    }
    run_all(&invocations, "test")
}

pub(crate) fn doc() -> Result<()> {
    let root = project_root();
    let registry = arch_registry(&root)?;
    let mut invocations = Vec::new();
    for workspace in workspaces(&root)? {
        invocations.extend(doc_commands(&workspace, &registry, &root));
    }
    run_all(&invocations, "doc")
}

fn selected_sides<'a>(args: &ClippyArgs, registry: &'a [ArchEntry]) -> Result<Vec<Side<'a>>> {
    let every_side = !args.host && !args.target && args.arch.is_empty();
    let mut sides = Vec::new();
    if args.host || every_side {
        sides.push(Side::Host);
    }
    if args.target || every_side {
        sides.extend(registry.iter().map(Side::Arch));
    }
    for name in &args.arch {
        let entry = registered(name, registry)?;
        if !sides.contains(&Side::Arch(entry)) {
            sides.push(Side::Arch(entry));
        }
    }
    Ok(sides)
}

fn cargo_in(root: &Path, manifest: &Path, subcommand: &str, args: &[&str]) -> Invocation {
    Invocation {
        args: [subcommand, "--manifest-path"]
            .into_iter()
            .map(String::from)
            .chain(std::iter::once(manifest.display().to_string()))
            .chain(args.iter().map(|arg| (*arg).to_string()))
            .collect(),
        dir: root.to_path_buf(),
        std_src: false,
        rustdoc_flags: false,
    }
}

fn clippy_commands(workspace: &Workspace, side: Side<'_>, root: &Path) -> Vec<Invocation> {
    let manifest = workspace.dir.join("Cargo.toml");
    let system_target = |entry: &ArchEntry| root.join(&entry.system).display().to_string();

    match (workspace.profile, side) {
        (Profile::Common | Profile::Host, Side::Host) => vec![cargo_in(
            root,
            &manifest,
            "clippy",
            &[
                "--workspace",
                "--all-targets",
                "--all-features",
                "--",
                "-D",
                "warnings",
            ],
        )],
        (Profile::Fuzz, Side::Host) => vec![cargo_in(
            root,
            &manifest,
            "clippy",
            &["--workspace", "--all-targets", "--", "-D", "warnings"],
        )],
        (Profile::Common, Side::Arch(entry)) => vec![cargo_in(
            root,
            &manifest,
            "clippy",
            &[
                "--workspace",
                "--target",
                &entry.bare,
                "--",
                "-D",
                "warnings",
            ],
        )],
        // `forced-target` каждого члена cargo применяет отдельно, и один вызов собирает
        // крейты всех архитектур.
        (Profile::Platform, Side::Arch(_)) => vec![cargo_in(
            root,
            &manifest,
            "clippy",
            &["--workspace", "--", "-D", "warnings"],
        )],
        (Profile::StdRuntime, Side::Arch(entry)) => vec![cargo_in(
            root,
            &manifest,
            "clippy",
            &[
                "--workspace",
                "--target",
                &system_target(entry),
                "-Zbuild-std=core,alloc",
                "-Zjson-target-spec",
                "--",
                "-D",
                "warnings",
            ],
        )],
        // Конфигурацию таргета cargo читает по рабочему каталогу, и команда идёт из каталога
        // workspace.
        (Profile::Userland, Side::Arch(entry)) => vec![Invocation {
            args: [
                "clippy",
                "--target",
                &system_target(entry),
                "--workspace",
                "--all-targets",
                "--",
                "-D",
                "warnings",
            ]
            .into_iter()
            .map(String::from)
            .collect(),
            dir: workspace.dir.clone(),
            std_src: true,
            rustdoc_flags: false,
        }],
        (Profile::Host | Profile::Fuzz, Side::Arch(_))
        | (Profile::Platform | Profile::StdRuntime | Profile::Userland, Side::Host) => Vec::new(),
    }
}

fn test_commands(workspace: &Workspace, root: &Path, host: &str) -> Vec<Invocation> {
    match workspace.profile {
        Profile::Common | Profile::Host => vec![cargo_in(
            root,
            &workspace.dir.join("Cargo.toml"),
            "test",
            &["--workspace", "--all-features"],
        )],
        // Примеры объявления компонента проверяют раскрытие макроса, и запускает их host-target.
        // Команда идёт из корня, потому что таргет userland задан конфигурацией его каталога.
        Profile::Userland => vec![cargo_in(
            root,
            &workspace.dir.join("Cargo.toml"),
            "test",
            &["-p", "component-startup", "--doc", "--target", host],
        )],
        // Платформенные крейты проверяет `cargo xtask qemu-test`, рантайм `std` и программы
        // образа - проход userland под QEMU, fuzz-цели - их прогон.
        Profile::Platform | Profile::StdRuntime | Profile::Fuzz => Vec::new(),
    }
}

fn doc_commands(workspace: &Workspace, registry: &[ArchEntry], root: &Path) -> Vec<Invocation> {
    let manifest = workspace.dir.join("Cargo.toml");
    let system_target = |entry: &ArchEntry| root.join(&entry.system).display().to_string();
    let with_flags = |mut invocation: Invocation| {
        invocation.rustdoc_flags = true;
        invocation
    };

    match workspace.profile {
        // `forced-target` членов `platform` cargo применяет и к rustdoc.
        Profile::Common | Profile::Host | Profile::Fuzz | Profile::Platform => vec![with_flags(
            cargo_in(root, &manifest, "doc", &["--no-deps", "--workspace"]),
        )],
        Profile::StdRuntime => registry
            .iter()
            .map(|entry| {
                with_flags(cargo_in(
                    root,
                    &manifest,
                    "doc",
                    &[
                        "--no-deps",
                        "--workspace",
                        "--target",
                        &system_target(entry),
                        "-Zbuild-std=core,alloc",
                        "-Zjson-target-spec",
                    ],
                ))
            })
            .collect(),
        Profile::Userland => registry
            .iter()
            .map(|entry| Invocation {
                args: [
                    "doc",
                    "--target",
                    &system_target(entry),
                    "--no-deps",
                    "--workspace",
                ]
                .into_iter()
                .map(String::from)
                .collect(),
                dir: workspace.dir.clone(),
                std_src: true,
                rustdoc_flags: true,
            })
            .collect(),
    }
}

fn run_all(invocations: &[Invocation], what: &str) -> Result<()> {
    if invocations.is_empty() {
        bail!("{what}: the selection names no command");
    }
    for invocation in invocations {
        println!("cargo {}", invocation.args.join(" "));
        let mut command = Command::new("cargo");
        command.args(&invocation.args).current_dir(&invocation.dir);
        if invocation.std_src {
            command.env("__CARGO_TESTS_ONLY_SRC_ROOT", userland::prepare_build()?);
        }
        if invocation.rustdoc_flags {
            command.env("RUSTDOCFLAGS", RUSTDOC_FLAGS);
        }
        shell::run(
            &mut command,
            &format!(
                "cargo {} in {}",
                invocation.args.join(" "),
                invocation.dir.display()
            ),
        )?;
    }
    println!("{what}: OK ({} commands)", invocations.len());
    Ok(())
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

    fn workspace(profile: Profile, dir: &str) -> Workspace {
        Workspace {
            dir: PathBuf::from(dir),
            profile,
        }
    }

    fn lines(invocations: &[Invocation]) -> Vec<String> {
        invocations
            .iter()
            .map(|invocation| {
                format!(
                    "{}: cargo {}{}{}",
                    invocation.dir.display(),
                    invocation.args.join(" "),
                    if invocation.std_src { " [std-src]" } else { "" },
                    if invocation.rustdoc_flags {
                        " [rustdoc-flags]"
                    } else {
                        ""
                    }
                )
            })
            .collect()
    }

    fn table(profile: Profile, dir: &str) -> (Vec<String>, Vec<String>, Vec<String>) {
        let registry = registry();
        let root = Path::new("/repo");
        let workspace = workspace(profile, dir);
        (
            lines(&clippy_commands(&workspace, Side::Host, root)),
            lines(&clippy_commands(&workspace, Side::Arch(&registry[0]), root)),
            lines(&test_commands(&workspace, root, "host-triple")),
        )
    }

    #[test]
    fn the_common_profile_lints_for_the_host_and_every_bare_target_and_tests_on_the_host() {
        let (host, arch, tests) = table(Profile::Common, "/repo");
        assert_eq!(
            host,
            [
                "/repo: cargo clippy --manifest-path /repo/Cargo.toml --workspace --all-targets --all-features -- -D warnings"
            ]
        );
        assert_eq!(
            arch,
            [
                "/repo: cargo clippy --manifest-path /repo/Cargo.toml --workspace --target aarch64-unknown-none -- -D warnings"
            ]
        );
        assert_eq!(
            tests,
            ["/repo: cargo test --manifest-path /repo/Cargo.toml --workspace --all-features"]
        );
    }

    #[test]
    fn the_host_profile_lints_and_tests_on_the_host_only() {
        let (host, arch, tests) = table(Profile::Host, "/repo/tools");
        assert_eq!(
            host,
            [
                "/repo: cargo clippy --manifest-path /repo/tools/Cargo.toml --workspace --all-targets --all-features -- -D warnings"
            ]
        );
        assert!(arch.is_empty());
        assert_eq!(
            tests,
            ["/repo: cargo test --manifest-path /repo/tools/Cargo.toml --workspace --all-features"]
        );
    }

    #[test]
    fn the_platform_profile_lints_the_whole_workspace_under_the_forced_targets() {
        let (host, arch, tests) = table(Profile::Platform, "/repo/kernel/platform");
        assert!(host.is_empty());
        assert_eq!(
            arch,
            [
                "/repo: cargo clippy --manifest-path /repo/kernel/platform/Cargo.toml --workspace -- -D warnings"
            ]
        );
        assert!(tests.is_empty());
    }

    #[test]
    fn the_std_runtime_profile_lints_under_the_system_target_with_core_from_source() {
        let (host, arch, tests) = table(Profile::StdRuntime, "/repo/lib/std-runtime");
        assert!(host.is_empty());
        assert_eq!(
            arch,
            [
                "/repo: cargo clippy --manifest-path /repo/lib/std-runtime/Cargo.toml --workspace --target /repo/targets/aarch64-unknown-molecular.json -Zbuild-std=core,alloc -Zjson-target-spec -- -D warnings"
            ]
        );
        assert!(tests.is_empty());
    }

    #[test]
    fn the_userland_profile_lints_from_its_directory_and_runs_its_doctests_on_the_host() {
        let (host, arch, tests) = table(Profile::Userland, "/repo/user");
        assert!(host.is_empty());
        assert_eq!(
            arch,
            [
                "/repo/user: cargo clippy --target /repo/targets/aarch64-unknown-molecular.json --workspace --all-targets -- -D warnings [std-src]"
            ]
        );
        assert_eq!(
            tests,
            [
                "/repo: cargo test --manifest-path /repo/user/Cargo.toml -p component-startup --doc --target host-triple"
            ]
        );
    }

    #[test]
    fn every_profile_documents_its_workspace_and_the_target_side_follows_the_registry() {
        let registry = registry();
        let root = Path::new("/repo");
        let doc = |profile, dir| lines(&doc_commands(&workspace(profile, dir), &registry, root));

        assert_eq!(
            doc(Profile::Common, "/repo"),
            [
                "/repo: cargo doc --manifest-path /repo/Cargo.toml --no-deps --workspace [rustdoc-flags]"
            ]
        );
        assert_eq!(
            doc(Profile::Platform, "/repo/kernel/platform"),
            [
                "/repo: cargo doc --manifest-path /repo/kernel/platform/Cargo.toml --no-deps --workspace [rustdoc-flags]"
            ]
        );
        assert_eq!(
            doc(Profile::StdRuntime, "/repo/lib/std-runtime"),
            [
                "/repo: cargo doc --manifest-path /repo/lib/std-runtime/Cargo.toml --no-deps --workspace --target /repo/targets/aarch64-unknown-molecular.json -Zbuild-std=core,alloc -Zjson-target-spec [rustdoc-flags]"
            ]
        );
        assert_eq!(
            doc(Profile::Userland, "/repo/user"),
            [
                "/repo/user: cargo doc --target /repo/targets/aarch64-unknown-molecular.json --no-deps --workspace [std-src] [rustdoc-flags]"
            ]
        );
    }

    #[test]
    fn a_second_architecture_adds_its_targets_without_a_new_command() {
        let mut registry = registry();
        registry.push(ArchEntry {
            name: "riscv64".to_string(),
            bare: "riscv64gc-unknown-none-elf".to_string(),
            system: "targets/riscv64-unknown-molecular.json".to_string(),
        });
        let args = ClippyArgs {
            host: false,
            target: true,
            arch: Vec::new(),
        };
        let sides = selected_sides(&args, &registry).unwrap();
        assert_eq!(sides, [Side::Arch(&registry[0]), Side::Arch(&registry[1])]);

        let workspace = workspace(Profile::Common, "/repo");
        let commands = clippy_commands(&workspace, sides[1], Path::new("/repo"));
        assert!(
            commands[0]
                .args
                .contains(&"riscv64gc-unknown-none-elf".to_string())
        );
    }
}
