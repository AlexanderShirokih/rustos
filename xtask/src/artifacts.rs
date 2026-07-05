//! Сборка артефактов ядра по `boot.format`: kernel.bin, kernel.gz, boot.img.

use std::{
    fs::File,
    io::{Read, Write},
    path::PathBuf,
    process::{Command, Stdio},
};

use anyhow::Result;
use flate2::{Compression, write::GzEncoder};

use crate::{context::BuildContext, shell::run_cmd};

fn protocol_feature(boot_format: &str) -> &'static str {
    match boot_format {
        "linux_arm64" | "android_boot_v1" | "android_boot_v2" => "boot-linux-arm64",
        "uefi" => "boot-uefi",
        other => {
            unreachable!("invalid boot.format '{other}' should have been rejected by load_spec")
        }
    }
}

fn build_features(ctx: &BuildContext) -> String {
    let mut features = vec![protocol_feature(ctx.spec.boot.format.as_str()).to_string()];
    features.extend(ctx.spec.features.iter().cloned());
    if let Some(extra) = ctx.features.as_deref()
        && !extra.is_empty()
    {
        features.push(extra.to_string());
    }
    features.join(",")
}

fn cargo_build(ctx: &BuildContext) -> Result<()> {
    let features = build_features(ctx);
    let mut cmd = Command::new("cargo");
    cmd.args([
        "build",
        "-p",
        "hal-aarch64",
        "--target",
        "aarch64-unknown-none",
        "--release",
        "--no-default-features",
        "--features",
        &features,
    ]);
    cmd.current_dir(&ctx.project_root)
        .env("DEVICE_SPEC", &ctx.spec_path)
        .stdout(Stdio::inherit())
        .stderr(Stdio::inherit());
    run_cmd(&mut cmd)?;
    Ok(())
}

fn make_kernel_bin(ctx: &BuildContext) -> Result<()> {
    let features = build_features(ctx);
    let mut cmd = Command::new("cargo");
    cmd.args([
        "objcopy",
        "--release",
        "-p",
        "hal-aarch64",
        "--target",
        "aarch64-unknown-none",
        "--no-default-features",
        "--features",
        &features,
    ]);
    cmd.args([
        "--",
        "--set-section-flags",
        ".bss=alloc,load,data",
        "-O",
        "binary",
    ])
    .arg(ctx.kernel_bin())
    .current_dir(&ctx.project_root)
    .env("DEVICE_SPEC", &ctx.spec_path)
    .stdout(Stdio::inherit())
    .stderr(Stdio::inherit());
    run_cmd(&mut cmd)?;
    Ok(())
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

fn make_boot_img_v1(ctx: &BuildContext) -> Result<()> {
    let dtb_path = ctx.dtb_path()?;

    // Вычисление kernel_offset для boot.img header
    let base = ctx.spec.boot.base.unwrap_or(0);
    let kernel_offset = ctx.spec.boot.offset.saturating_sub(base);
    let kernel_offset_str = format!("{kernel_offset:#x}");

    let mut output = File::create(ctx.kernel_gz_dtb())?;
    let mut kernel = Vec::new();
    let mut dtb = Vec::new();
    File::open(ctx.kernel_gz())?.read_to_end(&mut kernel)?;
    File::open(&dtb_path)?.read_to_end(&mut dtb)?;
    output.write_all(&kernel)?;
    output.write_all(&dtb)?;

    run_cmd(
        Command::new("mkbootimg")
            .args([
                "--kernel",
                ctx.kernel_gz_dtb().to_str().unwrap(),
                "--ramdisk",
                ctx.userland_img().to_str().unwrap(),
                "--base",
                "0x0",
                "--kernel_offset",
                &kernel_offset_str,
                "--ramdisk_offset",
                "0x01000000",
                "--pagesize",
                "4096",
                "--header_version",
                "1",
                "--output",
                ctx.boot_img().to_str().unwrap(),
            ])
            .current_dir(&ctx.project_root),
    )?;

    Ok(())
}

fn make_boot_img_v2(ctx: &BuildContext) -> Result<()> {
    let dtb_path = ctx.dtb_path()?;

    // Вычисление kernel_offset для boot.img header
    let base = ctx.spec.boot.base.unwrap_or(0);
    let kernel_offset = ctx.spec.boot.offset.saturating_sub(base);
    let kernel_offset_str = format!("{kernel_offset:#x}");

    run_cmd(
        Command::new("mkbootimg")
            .args([
                "--kernel",
                ctx.kernel_gz().to_str().unwrap(),
                "--ramdisk",
                ctx.userland_img().to_str().unwrap(),
                "--dtb",
                dtb_path.to_str().unwrap(),
                "--base",
                "0x0",
                "--kernel_offset",
                &kernel_offset_str,
                "--ramdisk_offset",
                "0x01000000",
                "--pagesize",
                "4096",
                "--header_version",
                "2",
                "--output",
                ctx.boot_img().to_str().unwrap(),
            ])
            .current_dir(&ctx.project_root),
    )?;

    Ok(())
}

pub(crate) fn build_binary(ctx: &BuildContext) -> Result<PathBuf> {
    cargo_build(ctx)?;
    make_kernel_bin(ctx)?;
    Ok(ctx.kernel_bin())
}

pub(crate) fn build_android(ctx: &BuildContext) -> Result<PathBuf> {
    cargo_build(ctx)?;
    make_kernel_bin(ctx)?;
    make_kernel_gz(ctx)?;

    match ctx.spec.boot.format.as_str() {
        "android_boot_v1" => make_boot_img_v1(ctx)?,
        "android_boot_v2" => make_boot_img_v2(ctx)?,
        _ => unreachable!(),
    }

    Ok(ctx.boot_img())
}
