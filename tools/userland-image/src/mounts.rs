use anyhow::{Context, Result, anyhow, ensure};
use filesystem_abi::{Mount, encode_mount_table};
use serde::Deserialize;

use crate::host::validate_image_path;

const MOUNT_TABLE_MANIFEST_VERSION: u16 = 1;

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct MountTableManifest {
    #[serde(default = "default_table_version")]
    version: u16,
    #[serde(default)]
    mounts: Vec<MountEntry>,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct MountEntry {
    path: String,
    program: String,
}

fn default_table_version() -> u16 {
    MOUNT_TABLE_MANIFEST_VERSION
}

pub(crate) fn compile_mount_table(source: &str, programs: &[&str]) -> Result<Vec<u8>> {
    let manifest = parse_mount_table(source)?;
    validate_mounts(&manifest, programs)?;
    let mounts: Vec<Mount<'_>> = manifest
        .mounts
        .iter()
        .map(|mount| Mount {
            path: &mount.path,
            program: &mount.program,
        })
        .collect();
    encode_mount_table(&mounts).map_err(|err| anyhow!("failed to serialize mount table: {err:?}"))
}

fn parse_mount_table(source: &str) -> Result<MountTableManifest> {
    let manifest: MountTableManifest =
        toml::from_str(source).context("Failed to parse mount table TOML")?;
    ensure!(
        manifest.version == MOUNT_TABLE_MANIFEST_VERSION,
        "Unsupported mount table version {}; expected {}",
        manifest.version,
        MOUNT_TABLE_MANIFEST_VERSION
    );
    Ok(manifest)
}

fn validate_mounts(manifest: &MountTableManifest, programs: &[&str]) -> Result<()> {
    for (index, mount) in manifest.mounts.iter().enumerate() {
        validate_image_path(&mount.path)
            .with_context(|| format!("mount '{}' is not a namespace prefix", mount.path))?;
        validate_image_path(&mount.program)
            .with_context(|| format!("mount '{}' names an invalid program", mount.path))?;
        ensure!(
            programs.contains(&mount.program.as_str()),
            "mount '{}' names program '{}' that the image does not contain",
            mount.path,
            mount.program
        );
        for earlier in &manifest.mounts[..index] {
            ensure!(
                earlier.path != mount.path,
                "mount table covers '{}' more than once",
                mount.path
            );
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use filesystem_abi::decode_mount_table;

    use super::*;

    const IMAGE_FS: &str = "/boot/filesystem-imagefs";
    const RAM_FS: &str = "/boot/filesystem-ramfs";

    const PROGRAMS: &[&str] = &[
        "/boot/rootkeeper",
        "/boot/namespace-router",
        IMAGE_FS,
        RAM_FS,
    ];

    fn boot_tree() -> String {
        format!(
            "version = 1\n\
             \n\
             [[mounts]]\n\
             path = \"/\"\n\
             program = \"{IMAGE_FS}\"\n\
             \n\
             [[mounts]]\n\
             path = \"/tmp\"\n\
             program = \"{RAM_FS}\"\n"
        )
    }

    fn rejected(source: &str) -> String {
        compile_mount_table(source, PROGRAMS)
            .expect_err("invalid mount table must fail")
            .to_string()
    }

    #[test]
    fn record_keeps_the_order_of_the_source() {
        let bytes = compile_mount_table(&boot_tree(), PROGRAMS).expect("compile must succeed");

        let table = decode_mount_table(&bytes).expect("compiled table must decode");
        let mounts: Vec<_> = table
            .iter()
            .map(|mount| (mount.path, mount.program))
            .collect();
        assert_eq!(mounts, [("/", IMAGE_FS), ("/tmp", RAM_FS)]);
    }

    #[test]
    fn empty_tree_compiles_to_a_table_without_mounts() {
        let bytes = compile_mount_table("version = 1\n", PROGRAMS).expect("compile must succeed");

        let table = decode_mount_table(&bytes).expect("compiled table must decode");
        assert!(table.is_empty());
    }

    #[test]
    fn mount_table_rejects_an_unsupported_version() {
        let source = boot_tree().replace("version = 1", "version = 2");
        assert_eq!(
            rejected(&source),
            "Unsupported mount table version 2; expected 1"
        );
    }

    #[test]
    fn mount_table_rejects_a_program_outside_the_image() {
        let source = boot_tree().replace(RAM_FS, "/boot/ghost-filesystem");
        assert_eq!(
            rejected(&source),
            "mount '/tmp' names program '/boot/ghost-filesystem' \
             that the image does not contain"
        );
    }

    #[test]
    fn mount_table_rejects_a_repeated_prefix() {
        let source = boot_tree().replace("path = \"/tmp\"", "path = \"/\"");
        assert_eq!(rejected(&source), "mount table covers '/' more than once");
    }

    #[test]
    fn mount_table_rejects_a_prefix_outside_the_contract() {
        let source = boot_tree().replace("path = \"/tmp\"", "path = \"tmp\"");
        assert!(rejected(&source).starts_with("mount 'tmp' is not a namespace prefix"));
    }

    #[test]
    fn mount_table_rejects_an_unknown_key() {
        let source = boot_tree().replace("path = \"/tmp\"", "readonly = true\npath = \"/tmp\"");
        assert!(rejected(&source).starts_with("Failed to parse mount table TOML"));
    }
}
