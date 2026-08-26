//! Host race-тесты production-примитивов под ThreadSanitizer.

use std::process::Command;

use anyhow::{Context, Result, bail};

use crate::{context::project_root, shell};

const TEST_FILTER: &str = "race_";

const PACKAGES: &[&str] = &[
    "capability",
    "collections",
    "hal-aarch64-asid",
    "memory",
    "scheduler",
];

pub(crate) fn run() -> Result<()> {
    let target = host_target()?;
    let project_root = project_root();
    let target_dir = project_root.join("target/race");
    let rustflags_key = format!(
        "CARGO_TARGET_{}_RUSTFLAGS",
        target.to_ascii_uppercase().replace('-', "_")
    );
    let mut command = Command::new("cargo");
    command.args([
        "test",
        "-Zbuild-std",
        "--target",
        &target,
        "--target-dir",
        target_dir
            .to_str()
            .context("Race target directory is not valid UTF-8")?,
    ]);
    for package in PACKAGES {
        command.args(["-p", package]);
    }
    command.args([TEST_FILTER, "--", "--test-threads=1"]);
    command
        .current_dir(project_root)
        .env(rustflags_key, "-Zsanitizer=thread")
        .env("CARGO_PROFILE_DEV_PANIC", "unwind");

    let mut executed = 0;
    let status = shell::run_streaming(&mut command, "cargo test", |line| {
        executed += passed_count(line).unwrap_or(0);
    })?;
    if !status.success() {
        bail!("Race tests failed with {status}");
    }
    if executed == 0 {
        bail!("Race tests matched no test - the '{TEST_FILTER}' filter selected nothing");
    }
    println!("race: {executed} tests passed under ThreadSanitizer");
    Ok(())
}

fn passed_count(line: &str) -> Option<usize> {
    if !line.starts_with("test result:") {
        return None;
    }
    let words: Vec<&str> = line.split_whitespace().collect();
    let index = words
        .iter()
        .position(|word| word.trim_end_matches(';') == "passed")?;
    words.get(index.checked_sub(1)?)?.parse().ok()
}

fn host_target() -> Result<String> {
    let mut command = Command::new("rustc");
    command.arg("-vV");
    let version = shell::output(&mut command, "rustc -vV")?;
    version
        .lines()
        .find_map(|line| line.strip_prefix("host: "))
        .map(str::to_owned)
        .context("rustc -vV did not report a host target")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_summary_line_yields_the_passed_count() {
        let line = "test result: ok. 12 passed; 0 failed; 0 ignored; 0 measured; 0 filtered out";
        assert_eq!(passed_count(line), Some(12));
    }

    #[test]
    fn a_line_that_is_not_a_summary_yields_nothing() {
        assert_eq!(passed_count("running 12 tests"), None);
        assert_eq!(passed_count("test race_handles ... ok"), None);
    }
}
