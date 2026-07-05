//! Спека устройства и контекст сборки: пути артефактов и корень workspace.

use std::{
    env, fs,
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

#[derive(Debug, Deserialize)]
pub(crate) struct BootSection {
    pub(crate) format: String,
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
    pub(crate) fn new(spec_path: PathBuf, features: Option<String>) -> Result<Self> {
        let project_root = project_root();

        let spec_path = if spec_path.is_absolute() {
            spec_path
        } else {
            project_root.join(&spec_path)
        };

        if !spec_path.exists() {
            bail!("Device spec '{}' not found", spec_path.display());
        }

        let spec = load_spec(&spec_path)?;

        let build_dir = project_root.join("target/build");
        fs::create_dir_all(&build_dir)?;

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
    let manifest_dir = env::var("CARGO_MANIFEST_DIR").unwrap_or_else(|_| ".".to_string());
    let path = Path::new(&manifest_dir);

    if path.ends_with("xtask") {
        path.parent().unwrap().to_path_buf()
    } else {
        path.to_path_buf()
    }
}

pub(crate) fn image_manifest_path(project_root: &Path, image: &Path) -> Result<PathBuf> {
    let path = if image.is_absolute() {
        image.to_path_buf()
    } else {
        project_root.join(image)
    };
    if !path.exists() {
        bail!("Userland image manifest not found at {}", path.display());
    }
    Ok(path)
}

fn load_spec(path: &Path) -> Result<DeviceSpec> {
    let file = File::open(path).context("Failed to open spec file")?;
    let spec: DeviceSpec = serde_yaml::from_reader(file).context("Failed to parse YAML")?;

    match spec.boot.format.as_str() {
        "linux_arm64" | "android_boot_v1" | "android_boot_v2" | "uefi" => {}
        other => bail!(
            "Unknown boot.format '{other}'. Expected: linux_arm64, android_boot_v1, android_boot_v2, uefi"
        ),
    }

    Ok(spec)
}
