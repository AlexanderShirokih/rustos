use anyhow::{Context, Result, anyhow, ensure};
use filesystem_abi::{Mount, Path, encode_mount_table};
use serde::Deserialize;

use crate::{
    host::validate_image_path,
    plan::{StageInstances, validate_name},
};

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
    instance: String,
}

fn default_table_version() -> u16 {
    MOUNT_TABLE_MANIFEST_VERSION
}

pub(crate) fn compile_mount_table(source: &str, instances: &StageInstances) -> Result<Vec<u8>> {
    let manifest = parse_mount_table(source)?;
    validate_mounts(&manifest, instances)?;
    let mounts: Vec<Mount<'_>> = manifest
        .mounts
        .iter()
        .map(|mount| Mount {
            path: &mount.path,
            instance: &mount.instance,
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

fn validate_mounts(manifest: &MountTableManifest, instances: &StageInstances) -> Result<()> {
    for (index, mount) in manifest.mounts.iter().enumerate() {
        validate_image_path(&mount.path)
            .with_context(|| format!("mount '{}' is not a namespace prefix", mount.path))?;
        for earlier in manifest.mounts.iter().take(index) {
            ensure!(
                earlier.path != mount.path,
                "mount table covers '{}' more than once",
                mount.path
            );
        }
        validate_name(&mount.instance)
            .with_context(|| format!("mount '{}' names an invalid instance", mount.path))?;
        validate_instance(mount, instances)?;
    }
    Ok(())
}

fn validate_instance(mount: &MountEntry, instances: &StageInstances) -> Result<()> {
    let stage_zero = instances.stage_zero.contains(&mount.instance);
    let stage_one = instances.stage_one.contains(&mount.instance);
    // Дерево прививается сессиями компонентов композиции: имени вне её взять сессию
    // владельцу политики загрузки негде.
    ensure!(
        stage_zero || stage_one,
        "mount '{}' names instance '{}', which the composition does not contain",
        mount.path,
        mount.instance
    );
    if mount.path == Path::ROOT.as_str() {
        // Корень прививается статически связанным слотом, а слот компонента ступени 0
        // называет провайдера своей ступени.
        ensure!(
            stage_zero,
            "root mount names instance '{}', which stage 1 raises; the boot policy owner grafts \
             the root with the session of a statically bound slot, and that slot names a \
             component of the boot plan",
            mount.instance
        );
        return Ok(());
    }
    // Сессию непокорневого монтирования владелец политики загрузки берёт у супервизора, а тот
    // отдаёт сессию компонента лишь тому, кто его сам определил.
    ensure!(
        stage_one,
        "mount '{}' names instance '{}', which the boot plan raises; the boot policy owner takes \
         the session of a mount other than the root from the supervisor, and the supervisor hands \
         a component's session only to the caller that defined it",
        mount.path,
        mount.instance
    );
    Ok(())
}

#[cfg(test)]
mod tests {
    use filesystem_abi::decode_mount_table;

    use super::*;

    const IMAGE_FS: &str = "filesystem-imagefs";
    const SPARE_FS: &str = "spare-filesystem";
    const RAM_FS: &str = "filesystem-ramfs";

    fn composition() -> StageInstances {
        StageInstances {
            stage_zero: vec![String::from(IMAGE_FS), String::from(SPARE_FS)],
            stage_one: vec![String::from(RAM_FS)],
        }
    }

    fn boot_tree() -> String {
        format!(
            "version = 1\n\
             \n\
             [[mounts]]\n\
             path = \"/\"\n\
             instance = \"{IMAGE_FS}\"\n\
             \n\
             [[mounts]]\n\
             path = \"/tmp\"\n\
             instance = \"{RAM_FS}\"\n"
        )
    }

    fn rejected(source: &str) -> String {
        compile_mount_table(source, &composition())
            .expect_err("invalid mount table must fail")
            .to_string()
    }

    #[test]
    fn record_keeps_the_order_of_the_source() {
        let bytes =
            compile_mount_table(&boot_tree(), &composition()).expect("compile must succeed");

        let table = decode_mount_table(&bytes).expect("compiled table must decode");
        let mounts: Vec<_> = table
            .iter()
            .map(|mount| (mount.path, mount.instance))
            .collect();
        assert_eq!(mounts, [("/", IMAGE_FS), ("/tmp", RAM_FS)]);
    }

    #[test]
    fn empty_tree_compiles_to_a_table_without_mounts() {
        let bytes =
            compile_mount_table("version = 1\n", &composition()).expect("compile must succeed");

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
    fn mount_table_rejects_a_program_path_in_place_of_an_instance() {
        let source = boot_tree().replace(RAM_FS, "/boot/filesystem-ramfs");
        assert_eq!(rejected(&source), "mount '/tmp' names an invalid instance");
    }

    #[test]
    fn mount_table_rejects_an_empty_instance() {
        let source = boot_tree().replace(RAM_FS, "");
        assert_eq!(rejected(&source), "mount '/tmp' names an invalid instance");
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
    fn mount_table_rejects_an_instance_outside_the_composition() {
        let source = boot_tree().replace(RAM_FS, "filesystem-ramsf");
        assert_eq!(
            rejected(&source),
            "mount '/tmp' names instance 'filesystem-ramsf', \
             which the composition does not contain"
        );
    }

    // Сессию непокорневого монтирования `init` берёт у супервизора, который отдаёт сессию
    // лишь определённого вызывателем компонента.
    #[test]
    fn mount_table_rejects_a_stage_zero_instance_below_the_root() {
        let source = boot_tree().replace(RAM_FS, SPARE_FS);
        assert_eq!(
            rejected(&source),
            "mount '/tmp' names instance 'spare-filesystem', which the boot plan raises; \
             the boot policy owner takes the session of a mount other than the root from the \
             supervisor, and the supervisor hands a component's session only to the caller \
             that defined it"
        );
    }

    #[test]
    fn mount_table_accepts_a_stage_one_instance_below_the_root() {
        let source = boot_tree().replace("path = \"/tmp\"", "path = \"/data\"");

        let bytes = compile_mount_table(&source, &composition()).expect("compile must succeed");

        let table = decode_mount_table(&bytes).expect("compiled table must decode");
        let mounts: Vec<_> = table
            .iter()
            .map(|mount| (mount.path, mount.instance))
            .collect();
        assert_eq!(mounts, [("/", IMAGE_FS), ("/data", RAM_FS)]);
    }

    #[test]
    fn mount_table_rejects_a_stage_one_instance_at_the_root() {
        let source = boot_tree().replace(IMAGE_FS, RAM_FS);
        assert_eq!(
            rejected(&source),
            "root mount names instance 'filesystem-ramfs', which stage 1 raises; \
             the boot policy owner grafts the root with the session of a statically bound slot, \
             and that slot names a component of the boot plan"
        );
    }

    #[test]
    fn mount_table_rejects_an_unknown_key() {
        let source = boot_tree().replace("path = \"/tmp\"", "readonly = true\npath = \"/tmp\"");
        assert!(rejected(&source).starts_with("Failed to parse mount table TOML"));
    }
}
