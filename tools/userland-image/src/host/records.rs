use std::{fs, path::Path};

use anyhow::{Context, Result, anyhow};
use userland_image::{decode_plan, export_entry_path};

use super::{files::LoadedFile, image::validate_image_path, source::ImagePlan};
use crate::plan::{PlanProgram, compile_plan, export_index};

pub(super) fn compile_plan_records(
    project_root: &Path,
    plan: &ImagePlan,
    programs: &[PlanProgram<'_>],
) -> Result<Vec<LoadedFile>> {
    validate_image_path(&plan.path)?;
    let source = project_root.join(&plan.source);
    let text = fs::read_to_string(&source).with_context(|| {
        format!(
            "Failed to read boot plan '{}' of image record '{}'",
            source.display(),
            plan.path
        )
    })?;
    let bytes = compile_plan(&text, programs)
        .with_context(|| format!("Failed to compile boot plan '{}'", source.display()))?;
    let decoded =
        decode_plan(&bytes).map_err(|error| anyhow!("compiled boot plan is invalid: {error:?}"))?;
    let index = export_index(&decoded, programs)
        .with_context(|| {
            format!(
                "Failed to index exports of boot plan '{}'",
                source.display()
            )
        })?
        .into_iter()
        .map(|entry| LoadedFile {
            path: export_entry_path(&plan.path, entry.protocol, &entry.name),
            source: source.clone(),
            bytes: entry.program.as_bytes().to_vec(),
        })
        .collect::<Vec<_>>();
    Ok(core::iter::once(LoadedFile {
        path: plan.path.clone(),
        source,
        bytes,
    })
    .chain(index)
    .collect())
}
