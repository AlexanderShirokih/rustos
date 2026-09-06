//! Сборка артефактов ядра по `boot.format`: kernel.bin, kernel.gz, boot.img.

use std::{
    fs::File,
    io::{Read, Write},
    path::{Path, PathBuf},
    process::{Command, Stdio},
};

use anyhow::{Context, Result};
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
    cmd.arg(subcommand)
        .arg("--manifest-path")
        .arg(&ctx.platform_manifest)
        .args([
            "-p",
            &ctx.kernel_package(),
            "--target",
            &ctx.arch.bare,
            "--release",
            "--no-default-features",
            "--features",
            features,
        ]);
    cmd.current_dir(&ctx.project_root)
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
        // Заголовок первой версии не несёт поля dtb, и дерево едет хвостом за ядром.
        BootImageVersion::V1 => {
            concat_files(&[&ctx.kernel_gz(), &dtb_path], &ctx.kernel_gz_dtb())?;
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
