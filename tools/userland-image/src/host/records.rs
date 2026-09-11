use std::{fs, path::Path};

use anyhow::{Context, Result};

use super::{files::LoadedFile, image::validate_image_path, source::ImagePlan};
use crate::plan::{PlanProgram, compile_plan};

pub(super) fn compile_plan_record(
    project_root: &Path,
    plan: &ImagePlan,
    programs: &[PlanProgram<'_>],
) -> Result<LoadedFile> {
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
    Ok(LoadedFile {
        path: plan.path.clone(),
        source,
        bytes,
    })
}
