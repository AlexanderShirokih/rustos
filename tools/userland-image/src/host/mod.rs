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

pub(crate) use self::image::validate_image_path;
pub use self::{build::UserlandTarget, files::ExtraFile};
use self::{
    build::build_program,
    files::collect_files,
    image::assemble_userland_image,
    program::resolve_programs,
    records::{compile_mount_table_record, compile_stage_records},
    source::load_manifest,
    workspace::{load_workspace_metadata, userland_workspace},
};
use crate::plan::{PlanProgram, StageInstances};

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
    let built_entries = programs
        .iter()
        .map(|program| {
            build_program(
                &userland_root,
                &workspace.target_directory,
                target,
                std_src,
                program,
                environment,
            )
        })
        .collect::<Result<Vec<_>>>()?;
    let built_programs: Vec<PlanProgram<'_>> = built_entries
        .iter()
        .map(|entry| PlanProgram {
            package: &entry.program.package,
            bin: &entry.program.bin,
            path: &entry.program.path,
            manifest: &entry.manifest,
        })
        .collect();
    let instances = if let Some(plan) = &manifest.plan {
        let stages = compile_stage_records(
            project_root,
            plan,
            manifest.stage1.as_ref(),
            &built_programs,
        )?;
        files.extend(stages.records);
        stages.instances
    } else {
        StageInstances::default()
    };
    for table in &manifest.mount_tables {
        files.push(compile_mount_table_record(project_root, table, &instances)?);
    }

    let blob = assemble_userland_image(&built_entries, &files)?;
    let output_path = build_dir.join("userland.img");
    fs::write(&output_path, blob)
        .with_context(|| format!("Failed to write userland image '{}'", output_path.display()))?;
    Ok(output_path)
}
