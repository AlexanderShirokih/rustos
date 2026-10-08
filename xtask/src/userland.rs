//! Сборка образа userland и прогон cargo в его workspace: и то, и другое идёт по
//! подготовленному дереву исходников `std` и собранной библиотеке C.
//!
//! Путь к дереву cargo принимает только переменной окружения: `[env]` из `.cargo/config.toml`
//! доходит до порождённых процессов, а до самого cargo - нет. Остальную конфигурацию таргета
//! cargo читает по рабочему каталогу, и команда идёт из каталога workspace.

use std::{
    path::{Path, PathBuf},
    process::Command,
};

use anyhow::{Context, Result};
use userland_image_tool::{
    BuildEnvironment, ExtraFile, USERLAND_WORKSPACE, UserlandTarget, build_userland,
};

use crate::{
    context::{build_dir, image_manifest_path, project_root},
    layers::{ArchEntry, arch_registry, registered},
    libc_src, shell, std_src,
};

pub(crate) fn run(args: &[String]) -> Result<()> {
    let workspace = project_root().join(USERLAND_WORKSPACE);
    let library = std_src::materialize(false)?;
    libc_src::materialize(false)?;
    let mut command = Command::new("cargo");
    command
        .args(args)
        .current_dir(&workspace)
        .env("__CARGO_TESTS_ONLY_SRC_ROOT", library);
    shell::run(&mut command, &format!("cargo in {}", workspace.display()))
}

pub(crate) fn run_for_arch(entry: &ArchEntry, args: &[String]) -> Result<()> {
    let system = project_root().join(&entry.system).display().to_string();
    let (subcommand, rest) = args
        .split_first()
        .context("cargo arguments name no subcommand")?;
    let mut with_target = vec![subcommand.clone(), "--target".to_string(), system];
    with_target.extend(rest.iter().cloned());
    run(&with_target)
}

pub(crate) fn build_image(
    project_root: &Path,
    build_dir: &Path,
    image: &Path,
    arch: &ArchEntry,
    environment: &[BuildEnvironment<'_>],
    files: &[ExtraFile],
) -> Result<PathBuf> {
    let manifest_path = image_manifest_path(project_root, image)?;
    let std_src = std_src::materialize(false)?;
    libc_src::materialize(false)?;
    let target = UserlandTarget::new(project_root.join(&arch.system), &arch.name)?;
    build_userland(
        project_root,
        &manifest_path,
        build_dir,
        &std_src,
        &target,
        environment,
        files,
    )
}

pub(crate) fn build_standalone_image(
    image: &Path,
    arch: Option<&str>,
    files: &[ExtraFile],
) -> Result<PathBuf> {
    let project_root = project_root();
    let build_dir = build_dir(&project_root)?;
    let registry = arch_registry(&project_root)?;
    let entry = match arch {
        Some(name) => registered(name, &registry)?,
        None => registry
            .first()
            .context("the architecture registry is empty")?,
    };
    build_image(&project_root, &build_dir, image, entry, &[], files)
}
