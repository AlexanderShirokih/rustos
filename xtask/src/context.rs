//! Спека устройства и контекст сборки: пути артефактов и корень workspace.

use std::{
    fs,
    fs::File,
    path::{Path, PathBuf},
};

use anyhow::{Context, Result, bail};
use serde::Deserialize;

#[derive(Debug, Deserialize)]
pub(crate) struct DeviceSpec {
    pub(crate) boot: BootSection,
    #[serde(default)]
    pub(crate) features: Vec<String>,
    #[serde(default)]
    pub(crate) run: Vec<String>,
    #[serde(default)]
    pub(crate) debug: Vec<String>,
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
}

pub(crate) struct BuildContext {
    pub(crate) project_root: PathBuf,
    pub(crate) build_dir: PathBuf,
    pub(crate) spec_path: PathBuf,
    pub(crate) spec: DeviceSpec,
    pub(crate) features: Option<String>,
}

impl BuildContext {
    pub(crate) fn new(spec_path: &Path, features: Option<String>) -> Result<Self> {
        let project_root = project_root();

        let spec_path = resolve(&project_root, spec_path, "Device spec")?;
        let spec = load_spec(&spec_path)?;

        let build_dir = build_dir(&project_root)?;

        Ok(Self {
            project_root,
            build_dir,
            spec_path,
            spec,
            features,
        })
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
