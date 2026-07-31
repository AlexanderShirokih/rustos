//! Ограниченные прогоны libFuzzer для недоверенных бинарных форматов.

use std::{
    fs,
    path::{Path, PathBuf},
    process::{Command, Stdio},
};

use anyhow::{Result, bail};

use crate::{context::project_root, shell::run_cmd};

const TARGETS: &[&str] = &["fdt_parse_walk", "ipc_wire_decode", "userland_image_decode"];

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
    fs::create_dir_all(fuzz_dir.join("corpus").join(target))?;
    let mut command = Command::new("cargo");
    command
        .args(["fuzz", "run", "--fuzz-dir"])
        .arg(&fuzz_dir)
        .arg(target)
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

    run_cmd(&mut command)?;
    Ok(())
}

fn dictionary_path(fuzz_dir: &Path, target: &str) -> Option<PathBuf> {
    let name = match target {
        "fdt_parse_walk" => "fdt.dict",
        "userland_image_decode" => "userland_image.dict",
        _ => return None,
    };
    Some(fuzz_dir.join("dictionaries").join(name))
}
