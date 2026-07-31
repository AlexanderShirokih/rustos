//! Host race-тесты production-примитивов под ThreadSanitizer.

use std::{
    io::{BufRead, BufReader},
    process::{Command, Stdio},
};

use anyhow::{Context, Result, bail};

use crate::context::project_root;

const TEST_FILTER: &str = "race_";

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
        "-p",
        "capability",
        TEST_FILTER,
        "--",
        "--test-threads=1",
    ]);
    command
        .current_dir(project_root)
        .env(rustflags_key, "-Zsanitizer=thread")
        .env("CARGO_PROFILE_DEV_PANIC", "unwind")
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::inherit());

    let mut child = command.spawn().context("Failed to launch cargo test")?;
    let stdout = child
        .stdout
        .take()
        .context("cargo test stdout not captured")?;
    let mut executed = 0;
    for line in BufReader::new(stdout).lines() {
        let line = line.context("Failed to read cargo test output")?;
        println!("{line}");
        executed += passed_count(&line).unwrap_or(0);
    }

    let status = child.wait().context("Failed to wait for cargo test")?;
    if !status.success() {
        bail!("Race tests failed with {status}");
    }
    if executed == 0 {
        bail!("Race tests matched no test - the '{TEST_FILTER}' filter selected nothing");
    }
    println!("race: {executed} tests passed under ThreadSanitizer");
    Ok(())
}

/// Число пройденных тестов из строки итога libtest.
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
    let output = Command::new("rustc")
        .arg("-vV")
        .output()
        .context("Failed to query rustc host target")?;
    if !output.status.success() {
        bail!("rustc -vV failed with {}", output.status);
    }
    let version = String::from_utf8(output.stdout).context("rustc -vV returned non-UTF8 output")?;
    version
        .lines()
        .find_map(|line| line.strip_prefix("host: "))
        .map(str::to_owned)
        .context("rustc -vV did not report a host target")
}
