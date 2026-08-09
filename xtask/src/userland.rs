//! Прогон cargo в workspace userspace: сборка идёт по подготовленному дереву исходников `std`.
//!
//! Путь к дереву cargo принимает только переменной окружения: `[env]` из `.cargo/config.toml`
//! доходит до порождённых процессов, а до самого cargo — нет. Остальную конфигурацию таргета
//! cargo читает по рабочему каталогу, и команда идёт из каталога workspace.

use std::process::Command;

use anyhow::{Context, Result, bail};
use userland_image_tool::USERLAND_WORKSPACE;

use crate::{context::project_root, std_src};

pub(crate) fn run(args: &[String]) -> Result<()> {
    let workspace = project_root().join(USERLAND_WORKSPACE);
    let library = std_src::materialize(false)?;
    let status = Command::new("cargo")
        .args(args)
        .current_dir(&workspace)
        .env("__CARGO_TESTS_ONLY_SRC_ROOT", library)
        .status()
        .context("Failed to run cargo in the userspace workspace")?;
    if !status.success() {
        bail!("cargo in {} failed with {status}", workspace.display());
    }
    Ok(())
}
