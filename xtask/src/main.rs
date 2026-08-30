//! CLI сборки и проверок ядра.

mod artifacts;
mod build;
mod check;
mod context;
mod fmt;
mod fuzz;
mod layers;
mod qemu_test;
mod race;
mod shell;
mod similarity;
mod std_src;
mod style;
mod userland;

use std::{path::PathBuf, process::ExitCode};

use anyhow::Result;
use clap::{Parser, Subcommand};
use userland_image_tool::ExtraFile;

use crate::{build::BuildArgs, check::CheckArgs, similarity::SimilarityArgs, style::StyleArgs};

const STD_VERIFY_ARGS: [&str; 4] = ["build", "--release", "--package", "hello-std"];

#[derive(Parser)]
#[command(name = "xtask")]
struct Cli {
    #[command(subcommand)]
    command: Commands,
}

#[derive(Subcommand)]
enum Commands {
    /// Run the main checks over the working tree
    Check(CheckArgs),
    /// Build the kernel for a device
    Build(BuildArgs),
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
        /// Operations in one epoch of a seed
        #[arg(long, default_value_t = 16)]
        epoch: u64,
        /// Index of this shard of the seed range
        #[arg(long, default_value_t = 0)]
        shard: u64,
        /// Number of shards the seed range is split into
        #[arg(long, default_value_t = 1)]
        shards: u64,
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
    Style(StyleArgs),
    /// Check the duplicate functions against the accepted baseline
    Similarity(SimilarityArgs),
}

fn main() -> Result<ExitCode> {
    let cli = Cli::parse();

    match cli.command {
        Commands::Check(args) => {
            let code = check::run(&args)?;
            if code != 0 {
                return Ok(ExitCode::from(code));
            }
        }
        Commands::Build(args) => build::run(args)?,
        Commands::QemuTest { timeout, spec } => qemu_test::qemu_test(timeout, &spec)?,
        Commands::Bench { timeout } => qemu_test::qemu_bench(timeout)?,
        Commands::Stress {
            seed,
            seeds,
            steps,
            epoch,
            shard,
            shards,
            timeout,
        } => qemu_test::qemu_stress(seed, seeds, steps, epoch, shard, shards, timeout)?,
        Commands::Race => race::run()?,
        Commands::Fuzz { target, seconds } => fuzz::run(&target, seconds)?,
        Commands::BuildUserland { image, files } => {
            let output = userland::build_standalone_image(&image, &files)?;
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
        Commands::Similarity(args) => similarity::run(args)?,
        Commands::Style(args) => {
            let code = style::run(args)?;
            if code != 0 {
                return Ok(ExitCode::from(u8::try_from(code).unwrap_or(1)));
            }
        }
    }

    Ok(ExitCode::SUCCESS)
}
