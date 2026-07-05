//! CLI сборки и проверок ядра.

mod artifacts;
mod context;
mod layers;
mod qemu_test;
mod shell;

use std::{
    fs,
    path::{Path, PathBuf},
};

use anyhow::{Result, bail};
use clap::{Parser, Subcommand};
use userland_image_tool::build_userland;

use crate::{
    artifacts::{build_android, build_binary},
    context::{BuildContext, image_manifest_path, project_root},
    shell::execute_commands,
};

#[derive(Parser)]
#[command(name = "xtask")]
struct Cli {
    #[command(subcommand)]
    command: Commands,
}

#[derive(Subcommand)]
enum Commands {
    /// Build the kernel for a device
    Build {
        /// Path to the device YAML spec
        spec: PathBuf,
        /// Run after the build (commands from 'run' in the YAML)
        #[arg(long)]
        run: bool,
        /// Debug after the build (commands from 'debug' in the YAML)
        #[arg(long)]
        debug: bool,
        /// Extra `hal-aarch64` cargo features (comma-separated)
        #[arg(long)]
        features: Option<String>,
    },
    /// Run the QEMU integration tests
    QemuTest {
        /// Timeout in seconds
        #[arg(long, default_value_t = 60)]
        timeout: u64,
    },
    /// Build userland.img from a TOML image manifest
    BuildUserland {
        /// Relative path to the image manifest
        #[arg(long, default_value = "user/rootkeeper/image.toml")]
        image: PathBuf,
    },
    /// Check the layer rules between workspace crates
    CheckLayers,
}

fn main() -> Result<()> {
    let cli = Cli::parse();

    match cli.command {
        Commands::Build {
            spec,
            run,
            debug,
            features,
        } => build(spec, run, debug, features)?,
        Commands::QemuTest { timeout } => qemu_test::qemu_test(timeout)?,
        Commands::BuildUserland { image } => {
            let project_root = project_root();
            let build_dir = project_root.join("target/build");
            fs::create_dir_all(&build_dir)?;
            let manifest_path = image_manifest_path(&project_root, &image)?;
            let output = build_userland(&project_root, &manifest_path, &build_dir)?;
            println!("Done: {}", output.display());
        }
        Commands::CheckLayers => layers::run()?,
    }

    Ok(())
}

fn build(spec: PathBuf, run: bool, debug: bool, features: Option<String>) -> Result<()> {
    let ctx = BuildContext::new(spec, features)?;
    let manifest_path =
        image_manifest_path(&ctx.project_root, Path::new("user/rootkeeper/image.toml"))?;
    build_userland(&ctx.project_root, &manifest_path, &ctx.build_dir)?;

    let output = match ctx.spec.boot.format.as_str() {
        "linux_arm64" => build_binary(&ctx)?,
        "android_boot_v1" | "android_boot_v2" => build_android(&ctx)?,
        "uefi" => bail!("UEFI boot not implemented yet"),
        _ => unreachable!(),
    };

    println!("Done: {}", output.display());

    if run {
        if ctx.spec.run.is_empty() {
            eprintln!("Warning: no 'run' commands in spec");
        } else {
            execute_commands(&ctx.spec.run, &ctx.project_root)?;
        }
    }

    if debug {
        if ctx.spec.debug.is_empty() {
            eprintln!("Warning: no 'debug' commands in spec");
        } else {
            execute_commands(&ctx.spec.debug, &ctx.project_root)?;
        }
    }

    Ok(())
}
