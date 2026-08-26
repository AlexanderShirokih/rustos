//! Ограниченные прогоны libFuzzer для недоверенных бинарных форматов.

use std::{
    fs,
    path::{Path, PathBuf},
    process::{Command, Stdio},
};

use anyhow::{Result, bail};
use userland_image_tool::userland_image_seeds;

use crate::{context::project_root, shell};

const TARGETS: &[&str] = &[
    "fdt_parse_walk",
    "ipc_wire_decode",
    "ramfs_ops",
    "userland_image_decode",
];

pub(crate) fn run(target: &str, seconds: u64) -> Result<()> {
    if !TARGETS.contains(&target) {
        bail!(
            "Unknown fuzz target '{target}'. Available targets: {}",
            TARGETS.join(", ")
        );
    }
    if seconds == 0 {
        bail!("Fuzz duration must be greater than zero");
    }

    let project_root = project_root();
    let fuzz_dir = project_root.join("fuzz");
    let corpus_dir = fuzz_dir.join("corpus").join(target);
    fs::create_dir_all(&corpus_dir)?;
    let seeds_dir = write_seeds(&fuzz_dir, target)?;

    let mut command = Command::new("cargo");
    command
        .args(["fuzz", "run", "--fuzz-dir"])
        .arg(&fuzz_dir)
        .arg(target)
        // Первая директория корпуса принимает новые юниты, остальные читаются
        // только на вход.
        .arg(&corpus_dir);
    if let Some(seeds_dir) = &seeds_dir {
        command.arg(seeds_dir);
    }
    command
        .arg("--")
        .arg(format!("-max_total_time={seconds}"))
        .arg("-timeout=10")
        .current_dir(&project_root)
        .stdin(Stdio::inherit())
        .stdout(Stdio::inherit())
        .stderr(Stdio::inherit());

    if let Some(dictionary) = dictionary_path(&fuzz_dir, target) {
        command.arg(format!("-dict={}", dictionary.display()));
    }

    shell::run(&mut command, "cargo fuzz run")
}

fn write_seeds(fuzz_dir: &Path, target: &str) -> Result<Option<PathBuf>> {
    let seeds = match target {
        "userland_image_decode" => userland_image_seeds()?,
        _ => return Ok(None),
    };

    let seeds_dir = fuzz_dir.join("seeds").join(target);
    if seeds_dir.exists() {
        fs::remove_dir_all(&seeds_dir)?;
    }
    fs::create_dir_all(&seeds_dir)?;
    for seed in seeds {
        fs::write(seeds_dir.join(seed.name), &seed.bytes)?;
    }
    Ok(Some(seeds_dir))
}

fn dictionary_path(fuzz_dir: &Path, target: &str) -> Option<PathBuf> {
    let name = match target {
        "fdt_parse_walk" => "fdt.dict",
        "userland_image_decode" => "userland_image.dict",
        _ => return None,
    };
    Some(fuzz_dir.join("dictionaries").join(name))
}
