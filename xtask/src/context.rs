//! Спека устройства и контекст сборки: пути артефактов, корень дерева, платформа ядра по
//! полю `arch` спеки и межпроцессный замок подготавливаемых деревьев `target/`.

use std::{
    fs,
    fs::{File, TryLockError},
    path::{Path, PathBuf},
};

use anyhow::{Context, Result, bail};
use serde::Deserialize;

use crate::layers::{self, ArchEntry, Profile};

#[derive(Debug, Deserialize)]
pub(crate) struct DeviceSpec {
    pub(crate) device: DeviceSection,
    pub(crate) boot: BootSection,
    #[serde(default)]
    pub(crate) features: Vec<String>,
    #[serde(default)]
    pub(crate) run: Vec<String>,
    #[serde(default)]
    pub(crate) debug: Vec<String>,
}

#[derive(Debug, Deserialize)]
pub(crate) struct DeviceSection {
    pub(crate) arch: String,
}

#[derive(Clone, Copy, Debug, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub(crate) enum BootFormat {
    LinuxArm64,
    AndroidBootV1,
    AndroidBootV2,
    Uefi,
}

#[derive(Debug, Deserialize)]
pub(crate) struct BootSection {
    pub(crate) format: BootFormat,
    pub(crate) offset: u64,
    pub(crate) dtb: Option<String>,
    pub(crate) base: Option<u64>,
    #[serde(default)]
    pub(crate) ext: BootExt,
}

#[derive(Debug, Default, Deserialize)]
pub(crate) struct BootExt {
    pub(crate) kernel_image_pad: Option<u64>,
}

pub(crate) struct BuildContext {
    pub(crate) project_root: PathBuf,
    pub(crate) build_dir: PathBuf,
    pub(crate) spec_path: PathBuf,
    pub(crate) spec: DeviceSpec,
    pub(crate) features: Option<String>,
    pub(crate) arch: ArchEntry,
    pub(crate) platform_dir: PathBuf,
}

impl BuildContext {
    pub(crate) fn new(spec_path: &Path, features: Option<String>) -> Result<Self> {
        let project_root = project_root();

        let spec_path = resolve(&project_root, spec_path, "Device spec")?;
        let spec = load_spec(&spec_path)?;
        let arch = spec_arch(&spec, &layers::arch_registry(&project_root)?)
            .with_context(|| spec_path.display().to_string())?;
        let platform_dir = layers::workspace_of_profile(&project_root, Profile::Platform)?;

        let build_dir = build_dir(&project_root)?;

        Ok(Self {
            project_root,
            build_dir,
            spec_path,
            spec,
            features,
            arch,
            platform_dir,
        })
    }

    pub(crate) fn kernel_package(&self) -> String {
        kernel_package(&self.arch)
    }

    pub(crate) fn kernel_bin(&self) -> PathBuf {
        self.build_dir.join("kernel.bin")
    }

    pub(crate) fn userland_img(&self) -> PathBuf {
        self.build_dir.join("userland.img")
    }

    pub(crate) fn kernel_gz(&self) -> PathBuf {
        self.build_dir.join("kernel.gz")
    }

    pub(crate) fn kernel_gz_dtb(&self) -> PathBuf {
        self.build_dir.join("kernel.gz+dtb")
    }

    pub(crate) fn boot_img(&self) -> PathBuf {
        self.build_dir.join("boot.img")
    }

    pub(crate) fn dtb_path(&self) -> Result<PathBuf> {
        let dtb = self
            .spec
            .boot
            .dtb
            .as_ref()
            .context("boot.dtb is required for android boot format")?;

        let path = self.project_root.join(dtb.trim_start_matches('/'));
        if !path.exists() {
            bail!("DTB not found at {}", path.display());
        }
        Ok(path)
    }
}

pub(crate) fn project_root() -> PathBuf {
    // Путь к манифесту крейта известен на этапе компиляции, и корень дерева не зависит от
    // каталога, из которого запущена команда.
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .unwrap_or(Path::new("."))
        .to_path_buf()
}

pub(crate) fn build_dir(project_root: &Path) -> Result<PathBuf> {
    let build_dir = project_root.join("target/build");
    fs::create_dir_all(&build_dir)
        .with_context(|| format!("Failed to create {}", build_dir.display()))?;
    Ok(build_dir)
}

pub(crate) fn lock_tree(path: &Path, label: &str) -> Result<File> {
    if let Some(dir) = path.parent() {
        fs::create_dir_all(dir).with_context(|| format!("Failed to create {}", dir.display()))?;
    }
    let file = File::create(path).with_context(|| format!("Failed to open {}", path.display()))?;

    match file.try_lock() {
        Ok(()) => {}
        Err(TryLockError::WouldBlock) => {
            println!("{label}: another xtask holds {}, waiting", path.display());
            file.lock()
                .with_context(|| format!("Failed to lock {}", path.display()))?;
        }
        Err(TryLockError::Error(error)) => {
            return Err(error).with_context(|| format!("Failed to lock {}", path.display()));
        }
    }
    Ok(file)
}

pub(crate) fn image_manifest_path(project_root: &Path, image: &Path) -> Result<PathBuf> {
    resolve(project_root, image, "Userland image manifest")
}

fn resolve(project_root: &Path, path: &Path, what: &str) -> Result<PathBuf> {
    let resolved = if path.is_absolute() {
        path.to_path_buf()
    } else {
        project_root.join(path)
    };
    if !resolved.exists() {
        bail!("{what} not found at {}", resolved.display());
    }
    Ok(resolved)
}

fn load_spec(path: &Path) -> Result<DeviceSpec> {
    let file = File::open(path).context("Failed to open spec file")?;
    serde_yaml::from_reader(file).context("Failed to parse YAML")
}

fn kernel_package(arch: &ArchEntry) -> String {
    format!("hal-{}", arch.name)
}

fn spec_arch(spec: &DeviceSpec, registry: &[ArchEntry]) -> Result<ArchEntry> {
    layers::registered(&spec.device.arch, registry)
        .cloned()
        .context("device.arch")
}

#[cfg(test)]
mod tests {
    use super::*;

    fn spec(arch: &str) -> DeviceSpec {
        let text = format!(
            "device:\n  name: Test\n  arch: {arch}\nboot:\n  format: linux_arm64\n  offset: 0x40200000\n"
        );
        serde_yaml::from_str(&text).expect("spec parses")
    }

    fn registry() -> Vec<ArchEntry> {
        vec![ArchEntry {
            name: "aarch64".to_string(),
            bare: "aarch64-unknown-none".to_string(),
            system: "targets/aarch64-unknown-molecular.json".to_string(),
        }]
    }

    #[test]
    fn a_second_architecture_selects_its_own_kernel_package_and_target() {
        let mut registry = registry();
        registry.push(ArchEntry {
            name: "riscv64".to_string(),
            bare: "riscv64gc-unknown-none-elf".to_string(),
            system: "targets/riscv64-unknown-molecular.json".to_string(),
        });
        let entry = spec_arch(&spec("riscv64"), &registry).unwrap();
        assert_eq!(kernel_package(&entry), "hal-riscv64");
        assert_eq!(entry.bare, "riscv64gc-unknown-none-elf");
    }

    #[test]
    fn the_lock_keeps_a_second_caller_out_until_the_guard_drops() {
        let root = std::env::temp_dir().join(format!("xtask-lock-tree-{}", std::process::id()));
        let path = root.join("tree.lock");

        let guard = lock_tree(&path, "test").unwrap();
        let second = File::open(&path).unwrap();
        assert!(matches!(second.try_lock(), Err(TryLockError::WouldBlock)));

        drop(guard);
        assert!(File::open(&path).unwrap().try_lock().is_ok());

        fs::remove_dir_all(&root).unwrap();
    }
}
