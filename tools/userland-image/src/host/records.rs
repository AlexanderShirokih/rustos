use std::{fs, path::Path};

use anyhow::{Context, Result};
use filesystem_abi::BOOT_MOUNT_TABLE_PATH;
use userland_image::BOOT_STAGE_ONE_DIRECTORY;

use super::{
    files::LoadedFile,
    image::validate_image_path,
    source::{ImageMountTable, ImagePlan, ImageStageOne},
};
use crate::{
    mounts::compile_mount_table,
    plan::{PlanProgram, StageInstances, compile_stages},
};

pub(super) struct StageRecords {
    pub(super) records: Vec<LoadedFile>,
    pub(super) instances: StageInstances,
}

pub(super) fn compile_stage_records(
    project_root: &Path,
    plan: &ImagePlan,
    stage_one: Option<&ImageStageOne>,
    programs: &[PlanProgram<'_>],
) -> Result<StageRecords> {
    validate_image_path(&plan.path)?;
    let plan_source = project_root.join(&plan.source);
    let plan_text = fs::read_to_string(&plan_source).with_context(|| {
        format!(
            "Failed to read boot plan '{}' of image record '{}'",
            plan_source.display(),
            plan.path
        )
    })?;
    let stage_one_source = stage_one.map(|stage| project_root.join(&stage.source));
    let stage_one_text = stage_one_source
        .as_ref()
        .map(|source| {
            fs::read_to_string(source)
                .with_context(|| format!("Failed to read stage 1 '{}'", source.display()))
        })
        .transpose()?;
    let stages =
        compile_stages(&plan_text, stage_one_text.as_deref(), programs).with_context(|| {
            match &stage_one_source {
                Some(stage) => format!(
                    "Failed to compile boot plan '{}' with stage 1 '{}'",
                    plan_source.display(),
                    stage.display()
                ),
                None => format!("Failed to compile boot plan '{}'", plan_source.display()),
            }
        })?;

    let mut records = vec![LoadedFile {
        path: plan.path.clone(),
        source: plan_source,
        bytes: stages.plan,
    }];
    for record in stages.stage_one {
        let path = format!("{BOOT_STAGE_ONE_DIRECTORY}/{}", record.name);
        validate_image_path(&path).context("stage 1 component names an invalid image record")?;
        records.push(LoadedFile {
            path,
            source: stage_one_source.clone().unwrap_or_default(),
            bytes: record.bytes,
        });
    }
    Ok(StageRecords {
        records,
        instances: stages.instances,
    })
}

pub(super) fn compile_mount_table_record(
    project_root: &Path,
    table: &ImageMountTable,
    instances: &StageInstances,
) -> Result<LoadedFile> {
    let source = project_root.join(&table.source);
    let text = fs::read_to_string(&source)
        .with_context(|| format!("Failed to read mount table '{}'", source.display()))?;
    let bytes = compile_mount_table(&text, instances)
        .with_context(|| format!("Failed to compile mount table '{}'", source.display()))?;
    Ok(LoadedFile {
        path: BOOT_MOUNT_TABLE_PATH.to_string(),
        source,
        bytes,
    })
}
