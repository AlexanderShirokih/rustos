//! Сборка артефактов ядра по `boot.format`: kernel.bin, kernel.gz, boot.img.

use std::{
    fs::{self, File, OpenOptions},
    io::{Read, Write},
    path::{Path, PathBuf},
    process::{Command, Stdio},
};

use anyhow::{Context, Result, bail};
use flate2::{Compression, write::GzEncoder};

use crate::{
    context::{BootFormat, BuildContext},
    shell,
};

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum BootImageVersion {
    V1,
    V2,
}

impl BootImageVersion {
    fn header_version(self) -> &'static str {
        match self {
            Self::V1 => "1",
            Self::V2 => "2",
        }
    }
}

fn protocol_feature(boot_format: BootFormat) -> &'static str {
    match boot_format {
        BootFormat::LinuxArm64 | BootFormat::AndroidBootV1 | BootFormat::AndroidBootV2 => {
            "boot-linux-arm64"
        }
        BootFormat::Uefi => "boot-uefi",
    }
}

fn build_features(ctx: &BuildContext) -> String {
    let mut features = vec![protocol_feature(ctx.spec.boot.format).to_string()];
    features.extend(ctx.spec.features.iter().cloned());
    if let Some(extra) = ctx.features.as_deref()
        && !extra.is_empty()
    {
        features.push(extra.to_string());
    }
    features.join(",")
}

fn hal_cargo(ctx: &BuildContext, subcommand: &str, features: &str) -> Command {
    let mut cmd = Command::new("cargo");
    cmd.arg(subcommand).args([
        "-p",
        &ctx.kernel_package(),
        "--target",
        &ctx.arch.bare,
        "--release",
        "--no-default-features",
        "--features",
        features,
    ]);
    // cargo objcopy разбирает аргументы своим парсером и не поддерживает --manifest-path,
    // поэтому платформенный workspace задаётся рабочим каталогом.
    cmd.current_dir(&ctx.platform_dir)
        .env("DEVICE_SPEC", &ctx.spec_path)
        .stdout(Stdio::inherit())
        .stderr(Stdio::inherit());
    cmd
}

fn cargo_build(ctx: &BuildContext) -> Result<()> {
    let mut cmd = hal_cargo(ctx, "build", &build_features(ctx));
    shell::run(
        &mut cmd,
        &format!("cargo build for {}", ctx.kernel_package()),
    )
}

fn make_kernel_bin(ctx: &BuildContext) -> Result<()> {
    let mut cmd = hal_cargo(ctx, "objcopy", &build_features(ctx));
    cmd.args([
        "--",
        "--set-section-flags",
        ".bss=alloc,load,data",
        "-O",
        "binary",
    ])
    .arg(ctx.kernel_bin());
    shell::run(
        &mut cmd,
        &format!("cargo objcopy for {}", ctx.kernel_package()),
    )
}

fn make_kernel_gz(ctx: &BuildContext) -> Result<()> {
    let mut input = Vec::new();
    File::open(ctx.kernel_bin())?.read_to_end(&mut input)?;

    let output = File::create(ctx.kernel_gz())?;
    let mut encoder = GzEncoder::new(output, Compression::best());
    encoder.write_all(&input)?;
    encoder.finish()?;

    Ok(())
}

fn make_boot_img(ctx: &BuildContext, version: BootImageVersion) -> Result<()> {
    let dtb_path = ctx.dtb_path()?;
    let base = ctx.spec.boot.base.unwrap_or(0);
    let kernel_offset = format!("{:#x}", ctx.spec.boot.offset.saturating_sub(base));

    let mut cmd = Command::new("mkbootimg");
    cmd.arg("--kernel");
    match version {
        // Заголовок первой версии не содержит поля dtb, и дерево присоединяется хвостом за ядром.
        BootImageVersion::V1 => {
            concat_files(&[&ctx.kernel_gz(), &dtb_path], &ctx.kernel_gz_dtb())?;

            if let Some(pad) = ctx.spec.boot.ext.kernel_image_pad {
                let image = fs::read(ctx.kernel_bin())
                    .with_context(|| format!("Failed to read {}", ctx.kernel_bin().display()))?;
                let block_size = kernel_block_size_past_padded_image(&image, pad)?;

                pad_file(&ctx.kernel_gz_dtb(), block_size)?;
            }

            cmd.arg(ctx.kernel_gz_dtb());
        }
        BootImageVersion::V2 => {
            cmd.arg(ctx.kernel_gz()).arg("--dtb").arg(&dtb_path);
        }
    }
    cmd.arg("--ramdisk")
        .arg(ctx.userland_img())
        .args([
            "--base",
            "0x0",
            "--kernel_offset",
            &kernel_offset,
            "--ramdisk_offset",
            "0x01000000",
            "--pagesize",
            "4096",
            "--header_version",
            version.header_version(),
        ])
        .arg("--output")
        .arg(ctx.boot_img())
        .current_dir(&ctx.project_root);
    shell::run(&mut cmd, "mkbootimg")
}

fn concat_files(sources: &[&Path], dest: &Path) -> Result<()> {
    let mut output =
        File::create(dest).with_context(|| format!("Failed to create {}", dest.display()))?;
    for source in sources {
        let mut bytes = Vec::new();
        File::open(source)
            .with_context(|| format!("Failed to open {}", source.display()))?
            .read_to_end(&mut bytes)
            .with_context(|| format!("Failed to read {}", source.display()))?;
        output
            .write_all(&bytes)
            .with_context(|| format!("Failed to write {}", dest.display()))?;
    }
    Ok(())
}

fn kernel_block_size_past_padded_image(image: &[u8], pad: u64) -> Result<u64> {
    if image.get(56..60) != Some(b"ARM\x64".as_slice()) {
        bail!("kernel.bin has no arm64 Image header");
    }

    let image_size = image
        .get(16..24)
        .and_then(|bytes| bytes.try_into().ok())
        .map(u64::from_le_bytes)
        .context("kernel.bin has no image_size in the arm64 Image header")?;

    Ok(image_size + pad)
}

fn pad_file(path: &Path, min_size: u64) -> Result<()> {
    let size = fs::metadata(path)
        .with_context(|| format!("Failed to read {}", path.display()))?
        .len();

    if size >= min_size {
        return Ok(());
    }

    let padding = usize::try_from(min_size - size)
        .with_context(|| format!("Padding of {} does not fit in memory", path.display()))?;

    OpenOptions::new()
        .append(true)
        .open(path)
        .with_context(|| format!("Failed to open {}", path.display()))?
        .write_all(&vec![0; padding])
        .with_context(|| format!("Failed to write {}", path.display()))
}

pub(crate) fn build_binary(ctx: &BuildContext) -> Result<PathBuf> {
    cargo_build(ctx)?;
    make_kernel_bin(ctx)?;
    Ok(ctx.kernel_bin())
}

pub(crate) fn build_android(ctx: &BuildContext, version: BootImageVersion) -> Result<PathBuf> {
    cargo_build(ctx)?;
    make_kernel_bin(ctx)?;
    make_kernel_gz(ctx)?;
    make_boot_img(ctx, version)?;
    Ok(ctx.boot_img())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn temp_file(name: &str, content: &[u8]) -> PathBuf {
        let path = std::env::temp_dir().join(format!("xtask-pad-{}-{name}", std::process::id()));
        fs::write(&path, content).unwrap();

        path
    }

    fn image_header(image_size: u64, magic: [u8; 4]) -> Vec<u8> {
        let mut header = vec![0; 64];
        header[16..24].copy_from_slice(&image_size.to_le_bytes());
        header[56..60].copy_from_slice(&magic);

        header
    }

    #[test]
    fn kernel_block_covers_image_size_and_pad() {
        let header = image_header(0x000c_a2d0, *b"ARM\x64");

        assert_eq!(
            kernel_block_size_past_padded_image(&header, 0x80000).unwrap(),
            0x0014_a2d0
        );
    }

    #[test]
    fn kernel_block_size_rejects_image_without_magic() {
        let header = image_header(0x000c_a2d0, [0; 4]);

        assert!(kernel_block_size_past_padded_image(&header, 0x80000).is_err());
    }

    #[test]
    fn pad_file_extends_short_file_with_zeros() {
        let path = temp_file("short", b"abc");

        pad_file(&path, 8).unwrap();

        assert_eq!(fs::read(&path).unwrap(), b"abc\0\0\0\0\0");
        fs::remove_file(path).unwrap();
    }

    #[test]
    fn pad_file_leaves_long_file_untouched() {
        let path = temp_file("long", b"abcdef");

        pad_file(&path, 4).unwrap();

        assert_eq!(fs::read(&path).unwrap(), b"abcdef");
        fs::remove_file(path).unwrap();
    }
}
