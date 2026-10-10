mod build;
mod files;
mod glob;
mod image;
mod program;
mod records;
mod source;
mod workspace;

use std::{
    fs,
    path::{Path, PathBuf},
};

use anyhow::{Context, Result};

pub use self::{build::UserlandTarget, files::ExtraFile};
use self::{
    build::build_programs,
    files::collect_files,
    image::assemble_userland_image,
    program::resolve_programs,
    records::compile_plan_records,
    source::load_manifest,
    workspace::{load_workspace_metadata, userland_workspace},
};
use crate::plan::PlanProgram;

/// Переменная окружения, передаваемая сборке каждой программы образа.
#[derive(Clone, Copy, Debug)]
pub struct BuildEnvironment<'a> {
    pub name: &'a str,
    pub value: &'a str,
}

/// Каталог workspace программ образа относительно корня проекта.
pub const USERLAND_WORKSPACE: &str = "user";

/// Собирает userland-образ под `target`, передавая `environment` каждому cargo-процессу
/// сборки. `extra_files` добавляются в образ поверх файлов манифеста, `std_src` - дерево
/// `library`, из которого сборка поднимает стандартную библиотеку.
pub fn build_userland(
    project_root: &Path,
    manifest_path: &Path,
    build_dir: &Path,
    std_src: &Path,
    target: &UserlandTarget,
    environment: &[BuildEnvironment<'_>],
    extra_files: &[ExtraFile],
) -> Result<PathBuf> {
    let manifest = load_manifest(manifest_path)?;
    let userland_root = userland_workspace(project_root);
    let workspace = load_workspace_metadata(&userland_root)?;
    let programs = resolve_programs(&manifest.programs, &workspace.packages)?;
    let mut files = collect_files(project_root, &manifest.files, extra_files)?;
    // План сверяется с манифестами компонентов, поэтому программы собираются до
    // его компиляции.
    let built_entries = build_programs(
        &userland_root,
        &workspace.target_directory,
        target,
        std_src,
        &programs,
        environment,
    )?;
    let built_programs: Vec<PlanProgram<'_>> = built_entries
        .iter()
        .map(|entry| PlanProgram {
            package: &entry.program.package,
            bin: &entry.program.bin,
            path: &entry.program.path,
            boot: entry.program.boot,
            data: &entry.bytes,
        })
        .collect();
    if let Some(plan) = &manifest.plan {
        files.extend(compile_plan_records(project_root, plan, &built_programs)?);
    }

    let blob = assemble_userland_image(&built_entries, &files)?;
    let output_path = build_dir.join("userland.img");
    fs::write(&output_path, blob)
        .with_context(|| format!("Failed to write userland image '{}'", output_path.display()))?;
    Ok(output_path)
}

/// Отдаёт пути `source` из `[[files]]` манифеста образа `manifest_path` в записанном виде.
/// Относительный путь отсчитывается от корня проекта, путь может содержать шаблон.
pub fn manifest_file_sources(manifest_path: &Path) -> Result<Vec<PathBuf>> {
    let manifest = load_manifest(manifest_path)?;
    Ok(manifest.files.into_iter().map(|file| file.source).collect())
}
