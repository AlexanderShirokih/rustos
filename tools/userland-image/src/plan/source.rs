use anyhow::{Context, Result, ensure};
use serde::Deserialize;

pub(super) const BOOT_PLAN_MANIFEST_VERSION: u16 = 1;

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub(super) struct SystemManifest {
    #[serde(default = "default_plan_version")]
    version: u16,
    pub(super) components: Vec<ComponentEntry>,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub(super) struct ComponentEntry {
    pub(super) executable: String,
    #[serde(default)]
    pub(super) name: Option<String>,
    pub(super) memory_limit: String,
    #[serde(default)]
    pub(super) restarts: u32,
    #[serde(default)]
    pub(super) mountpoint: Option<String>,
}

fn default_plan_version() -> u16 {
    BOOT_PLAN_MANIFEST_VERSION
}

pub(super) fn parse_system_manifest(source: &str) -> Result<SystemManifest> {
    let manifest: SystemManifest =
        toml::from_str(source).context("Failed to parse boot plan TOML")?;
    ensure!(
        manifest.version == BOOT_PLAN_MANIFEST_VERSION,
        "Unsupported boot plan version {}; expected {}",
        manifest.version,
        BOOT_PLAN_MANIFEST_VERSION
    );
    ensure!(
        !manifest.components.is_empty(),
        "boot plan must contain at least one component; found none"
    );
    Ok(manifest)
}
