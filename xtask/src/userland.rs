//! Сборка образа userland и прогон cargo в его workspace: и то, и другое идёт по
//! подготовленному дереву исходников `std`.
//!
//! Путь к дереву cargo принимает только переменной окружения: `[env]` из `.cargo/config.toml`
//! доходит до порождённых процессов, а до самого cargo — нет. Остальную конфигурацию таргета
//! cargo читает по рабочему каталогу, и команда идёт из каталога workspace.

use std::{
    path::{Path, PathBuf},
    process::Command,
};

use anyhow::Result;
use userland_image_tool::{BuildEnvironment, ExtraFile, USERLAND_WORKSPACE, build_userland};

use crate::{
    context::{build_dir, image_manifest_path, project_root},
    shell, std_src,
};

pub(crate) fn run(args: &[String]) -> Result<()> {
    let workspace = project_root().join(USERLAND_WORKSPACE);
    let library = std_src::materialize(false)?;
    let mut command = Command::new("cargo");
    command
        .args(args)
        .current_dir(&workspace)
        .env("__CARGO_TESTS_ONLY_SRC_ROOT", library);
    shell::run(&mut command, &format!("cargo in {}", workspace.display()))
}

pub(crate) fn build_image(
    project_root: &Path,
    build_dir: &Path,
    image: &Path,
    environment: &[BuildEnvironment<'_>],
    files: &[ExtraFile],
) -> Result<PathBuf> {
    let manifest_path = image_manifest_path(project_root, image)?;
    let std_src = std_src::materialize(false)?;
    build_userland(
        project_root,
        &manifest_path,
        build_dir,
        &std_src,
        environment,
        files,
    )
}

pub(crate) fn build_standalone_image(image: &Path, files: &[ExtraFile]) -> Result<PathBuf> {
    let project_root = project_root();
    let build_dir = build_dir(&project_root)?;
    build_image(&project_root, &build_dir, image, &[], files)
}
