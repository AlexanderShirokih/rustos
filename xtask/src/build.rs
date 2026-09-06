//! Сборка образа устройства по его спеке: образ userland, артефакт ядра и запуск команд спеки.

use std::path::PathBuf;

use anyhow::{Result, bail};
use clap::Args;
use userland_image_tool::ExtraFile;

use crate::{
    artifacts::{BootImageVersion, build_android, build_binary},
    context::{BootFormat, BuildContext},
    shell::execute_commands,
    userland,
};

#[derive(Args)]
pub(crate) struct BuildArgs {
    /// Path to the device YAML spec
    spec: PathBuf,
    /// Run after the build (commands from 'run' in the YAML)
    #[arg(long)]
    run: bool,
    /// Debug after the build (commands from 'debug' in the YAML)
    #[arg(long)]
    debug: bool,
    /// Extra kernel cargo features (comma-separated)
    #[arg(long)]
    features: Option<String>,
    /// Relative path to the userland image manifest
    #[arg(long, default_value = "user/rootkeeper/image.toml")]
    image: PathBuf,
    /// Extra image file as 'path=source'; repeat for several files
    #[arg(long = "file", value_name = "PATH=SOURCE")]
    files: Vec<ExtraFile>,
}

pub(crate) fn run(args: BuildArgs) -> Result<()> {
    let ctx = BuildContext::new(&args.spec, args.features)?;
    userland::build_image(
        &ctx.project_root,
        &ctx.build_dir,
        &args.image,
        &ctx.arch,
        &[],
        &args.files,
    )?;

    let output = match ctx.spec.boot.format {
        BootFormat::LinuxArm64 => build_binary(&ctx)?,
        BootFormat::AndroidBootV1 => build_android(&ctx, BootImageVersion::V1)?,
        BootFormat::AndroidBootV2 => build_android(&ctx, BootImageVersion::V2)?,
        BootFormat::Uefi => bail!("UEFI boot not implemented yet"),
    };
    println!("Done: {}", output.display());

    if args.run {
        run_spec_commands(&ctx.spec.run, "run", &ctx)?;
    }
    if args.debug {
        run_spec_commands(&ctx.spec.debug, "debug", &ctx)?;
    }
    Ok(())
}

fn run_spec_commands(commands: &[String], section: &str, ctx: &BuildContext) -> Result<()> {
    if commands.is_empty() {
        eprintln!("Warning: no '{section}' commands in spec");
        return Ok(());
    }
    execute_commands(commands, &ctx.project_root)
}
