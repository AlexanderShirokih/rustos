use std::{
    fs,
    path::{Path, PathBuf},
};

use anyhow::{Context, Result, ensure};
use serde::Deserialize;
use userland_image::{BOOT_PLAN_PATH, USERLAND_IMAGE_VERSION};

pub(super) const USERLAND_MANIFEST_VERSION: u16 = USERLAND_IMAGE_VERSION;

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub(super) struct ImageManifest {
    #[serde(default = "default_manifest_version")]
    pub(super) version: u16,
    pub(super) programs: Vec<ImageProgram>,
    #[serde(default)]
    pub(super) files: Vec<ImageFile>,
    pub(super) plan: Option<ImagePlan>,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub(super) struct ImagePlan {
    pub(super) source: PathBuf,
    #[serde(default = "default_plan_path")]
    pub(super) path: String,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub(super) struct ImageProgram {
    pub(super) package: String,
    pub(super) path: String,
    pub(super) bin: Option<String>,
    pub(super) boot: Option<bool>,
    pub(super) stack_size: Option<u64>,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub(super) struct ImageFile {
    pub(super) path: String,
    pub(super) source: PathBuf,
}

fn default_manifest_version() -> u16 {
    USERLAND_MANIFEST_VERSION
}

fn default_plan_path() -> String {
    BOOT_PLAN_PATH.to_string()
}

pub(super) fn load_manifest(path: &Path) -> Result<ImageManifest> {
    let manifest_src = fs::read_to_string(path).with_context(|| {
        format!(
            "Failed to read userland image manifest '{}'",
            path.display()
        )
    })?;
    parse_manifest(&manifest_src)
}

pub(super) fn parse_manifest(src: &str) -> Result<ImageManifest> {
    let manifest: ImageManifest =
        toml::from_str(src).context("Failed to parse userland image manifest TOML")?;
    ensure!(
        manifest.version == USERLAND_MANIFEST_VERSION,
        "Unsupported userland image manifest version {}; expected {}",
        manifest.version,
        USERLAND_MANIFEST_VERSION
    );
    ensure!(
        !manifest.programs.is_empty(),
        "userland image manifest must contain at least one [[programs]] item"
    );
    Ok(manifest)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn manifest_parses_programs_and_files() {
        let manifest = parse_manifest(
            "version = 1\n\
             \n\
             [[programs]]\n\
             package = \"rootkeeper\"\n\
             path = \"/boot/rootkeeper\"\n\
             boot = true\n\
             \n\
             [[programs]]\n\
             package = \"testrunner\"\n\
             path = \"/boot/tests\"\n\
             stack_size = 131072\n\
             \n\
             [[files]]\n\
             path = \"/data/sample\"\n\
             source = \"user/rootkeeper/sample.txt\"\n",
        )
        .expect("composition manifest must parse");

        assert_eq!(manifest.version, 1);
        assert_eq!(manifest.programs.len(), 2);
        assert_eq!(manifest.programs[0].path, "/boot/rootkeeper");
        assert_eq!(manifest.programs[0].boot, Some(true));
        assert_eq!(manifest.programs[1].stack_size, Some(131_072));
        assert_eq!(manifest.files[0].path, "/data/sample");
        assert_eq!(
            manifest.files[0].source,
            PathBuf::from("user/rootkeeper/sample.txt")
        );
    }

    #[test]
    fn manifest_parses_a_source_pattern() {
        let manifest = parse_manifest(
            "version = 1\n\
             \n\
             [[programs]]\n\
             package = \"rootkeeper\"\n\
             path = \"/boot/rootkeeper\"\n\
             boot = true\n\
             \n\
             [[files]]\n\
             path = \"/data/assets\"\n\
             source = \"user/rootkeeper/assets/**/*.txt\"\n",
        )
        .expect("composition manifest must parse");

        assert_eq!(manifest.files[0].path, "/data/assets");
        assert_eq!(
            manifest.files[0].source,
            PathBuf::from("user/rootkeeper/assets/**/*.txt")
        );
    }

    #[test]
    fn manifest_rejects_program_without_path() {
        let err = parse_manifest("version = 1\n\n[[programs]]\npackage = \"rootkeeper\"\n")
            .expect_err("path is required");
        assert!(
            err.to_string()
                .contains("Failed to parse userland image manifest TOML")
        );
    }

    #[test]
    fn manifest_rejects_entry_name_override() {
        let err = parse_manifest(
            "version = 1\n\
             \n\
             [[programs]]\n\
             package = \"rootkeeper\"\n\
             path = \"/boot/rootkeeper\"\n\
             name = \"rootkeeper\"\n",
        )
        .expect_err("name override must fail");
        assert!(
            err.to_string()
                .contains("Failed to parse userland image manifest TOML")
        );
    }

    #[test]
    fn manifest_rejects_unsupported_version() {
        let err = parse_manifest(
            "version = 2\n\n[[programs]]\npackage = \"rootkeeper\"\npath = \"/boot/rootkeeper\"\n",
        )
        .expect_err("unsupported version must fail");
        assert_eq!(
            err.to_string(),
            "Unsupported userland image manifest version 2; expected 1"
        );
    }

    #[test]
    fn manifest_rejects_empty_programs() {
        let err = parse_manifest("version = 1\nprograms = []\n")
            .expect_err("empty composition must fail");
        assert_eq!(
            err.to_string(),
            "userland image manifest must contain at least one [[programs]] item"
        );
    }
}
