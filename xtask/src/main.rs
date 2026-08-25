//! CLI сборки и проверок ядра.

mod artifacts;
mod context;
mod fmt;
mod fuzz;
mod layers;
mod qemu_test;
mod race;
mod shell;
mod std_src;
mod style;
mod userland;

use std::{
    fs,
    path::{Path, PathBuf},
};

use anyhow::{Result, bail};
use clap::{Parser, Subcommand};
use userland_image_tool::{ExtraFile, build_userland};

use crate::{
    artifacts::{build_android, build_binary},
    context::{BuildContext, image_manifest_path, project_root},
    shell::execute_commands,
};

const STD_VERIFY_ARGS: [&str; 4] = ["build", "--release", "--package", "hello-std"];

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
        /// Relative path to the userland image manifest
        #[arg(long, default_value = "user/rootkeeper/image.toml")]
        image: PathBuf,
        /// Extra image file as 'path=source'; repeat for several files
        #[arg(long = "file", value_name = "PATH=SOURCE")]
        files: Vec<ExtraFile>,
    },
    /// Run the QEMU integration tests
    QemuTest {
        /// Timeout in seconds
        #[arg(long, default_value_t = 60)]
        timeout: u64,
        /// Device spec to run against
        #[arg(long, default_value = qemu_test::DEFAULT_SPEC)]
        spec: PathBuf,
    },
    /// Run the userspace filesystem benchmarks in QEMU
    Bench {
        /// Timeout in seconds
        #[arg(long, default_value_t = 120)]
        timeout: u64,
    },
    /// Run reproducible userspace stress tests in QEMU
    Stress {
        /// First deterministic seed
        #[arg(long, default_value_t = 1)]
        seed: u64,
        /// Number of consecutive seeds
        #[arg(long, default_value_t = 4)]
        seeds: u64,
        /// Operations generated for each seed
        #[arg(long, default_value_t = 64)]
        steps: u64,
        /// Timeout in seconds
        #[arg(long, default_value_t = 120)]
        timeout: u64,
    },
    /// Run host concurrency tests under ThreadSanitizer
    Race,
    /// Run one bounded libFuzzer target
    Fuzz {
        /// Fuzz target name, as listed by `cargo fuzz list --fuzz-dir fuzz`
        target: String,
        /// Maximum fuzzing time in seconds
        #[arg(long, default_value_t = 60)]
        seconds: u64,
    },
    /// Build userland.img from a TOML image manifest
    BuildUserland {
        /// Relative path to the image manifest
        #[arg(long, default_value = "user/rootkeeper/image.toml")]
        image: PathBuf,
        /// Extra image file as 'path=source'; repeat for several files
        #[arg(long = "file", value_name = "PATH=SOURCE")]
        files: Vec<ExtraFile>,
    },
    /// Run cargo in the userspace workspace, against the patched `std` sources
    Userland {
        /// cargo arguments, for example `clippy --workspace -- -D warnings`
        #[arg(trailing_var_arg = true, allow_hyphen_values = true, num_args = 1..)]
        args: Vec<String>,
    },
    /// Run rustfmt over the root workspace and every detached one
    Fmt {
        /// Report the files that need formatting instead of rewriting them
        #[arg(long)]
        check: bool,
    },
    /// Check the layer rules between workspace crates
    CheckLayers,
    /// Materialize the patched `std` sources under target/std-src
    StdSrc {
        /// Rebuild the tree even when the overlay and the toolchain are unchanged
        #[arg(long)]
        force: bool,
        /// Build the image program that runs on `std` to prove the tree works
        #[arg(long)]
        verify: bool,
    },
    /// Check the code style conventions
    Style {
        /// Check the whole tree instead of the changed lines
        #[arg(long)]
        all: bool,
        /// Check the staged contents, for the pre-commit hook
        #[arg(long)]
        staged: bool,
        /// Compare against a revision instead of the default base
        #[arg(long, value_name = "REV")]
        since: Option<String>,
        /// Regenerate the baseline snapshot from the current tree
        #[arg(long)]
        write_baseline: bool,
        /// Report findings accepted by the baseline as well
        #[arg(long)]
        no_baseline: bool,
        /// Output format
        #[arg(long, default_value = "text")]
        format: String,
        /// Restrict the run to the listed rules
        #[arg(long = "rule", value_name = "ID")]
        rules: Vec<String>,
    },
}

fn main() -> Result<()> {
    let cli = Cli::parse();

    match cli.command {
        Commands::Build {
            spec,
            run,
            debug,
            features,
            image,
            files,
        } => build(spec, run, debug, features, &image, &files)?,
        Commands::QemuTest { timeout, spec } => qemu_test::qemu_test(timeout, &spec)?,
        Commands::Bench { timeout } => qemu_test::qemu_bench(timeout)?,
        Commands::Stress {
            seed,
            seeds,
            steps,
            timeout,
        } => qemu_test::qemu_stress(seed, seeds, steps, timeout)?,
        Commands::Race => race::run()?,
        Commands::Fuzz { target, seconds } => fuzz::run(&target, seconds)?,
        Commands::BuildUserland { image, files } => {
            let project_root = project_root();
            let build_dir = project_root.join("target/build");
            fs::create_dir_all(&build_dir)?;
            let manifest_path = image_manifest_path(&project_root, &image)?;
            let std_src = std_src::materialize(false)?;
            let output = build_userland(
                &project_root,
                &manifest_path,
                &build_dir,
                &std_src,
                &[],
                &files,
            )?;
            println!("Done: {}", output.display());
        }
        Commands::Userland { args } => userland::run(&args)?,
        Commands::Fmt { check } => fmt::run(check)?,
        Commands::CheckLayers => layers::run()?,
        Commands::StdSrc { force, verify } => {
            std_src::run(force)?;
            if verify {
                // Оверлей обязан не только ложиться, но и давать дерево, из которого
                // собирается программа образа.
                userland::run(&STD_VERIFY_ARGS.map(String::from))?;
            }
        }
        Commands::Style {
            all,
            staged,
            since,
            write_baseline,
            no_baseline,
            format,
            rules,
        } => style::run(&style::Options {
            all,
            staged,
            since,
            baseline: match (write_baseline, no_baseline) {
                (true, _) => style::BaselineMode::Write,
                (false, true) => style::BaselineMode::Ignore,
                (false, false) => style::BaselineMode::Apply,
            },
            format,
            rules,
        })?,
    }

    Ok(())
}

fn build(
    spec: PathBuf,
    run: bool,
    debug: bool,
    features: Option<String>,
    image: &Path,
    files: &[ExtraFile],
) -> Result<()> {
    let ctx = BuildContext::new(spec, features)?;
    let manifest_path = image_manifest_path(&ctx.project_root, image)?;
    let std_src = std_src::materialize(false)?;
    build_userland(
        &ctx.project_root,
        &manifest_path,
        &ctx.build_dir,
        &std_src,
        &[],
        files,
    )?;

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
