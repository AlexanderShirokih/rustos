//! Утилиты запуска внешних команд проверяют код возврата, а имя команды включают в сообщение
//! об отказе.
//!
//! `run_streaming` отдаёт код возврата вызывающему. У прогонов под QEMU и под санитайзером
//! маркеры вывода и код разбираются вместе.

use std::{
    io::{BufRead, BufReader},
    path::Path,
    process::{Command, ExitStatus, Stdio},
};

use anyhow::{Context, Result, bail};

pub(crate) fn run(cmd: &mut Command, what: &str) -> Result<()> {
    let status = cmd
        .status()
        .with_context(|| format!("Failed to run {what}"))?;
    if !status.success() {
        bail!("{what} failed with {status}");
    }
    Ok(())
}

pub(crate) fn output(cmd: &mut Command, what: &str) -> Result<String> {
    let output = cmd
        .output()
        .with_context(|| format!("Failed to run {what}"))?;
    if !output.status.success() {
        bail!("{what} failed with {}", output.status);
    }
    String::from_utf8(output.stdout).with_context(|| format!("{what} returned non-UTF-8 output"))
}

pub(crate) fn run_streaming(
    cmd: &mut Command,
    what: &str,
    mut observe: impl FnMut(&str),
) -> Result<ExitStatus> {
    let mut child = cmd
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::inherit())
        .spawn()
        .with_context(|| format!("Failed to launch {what}"))?;
    let stdout = child
        .stdout
        .take()
        .with_context(|| format!("{what} stdout not captured"))?;
    for line in BufReader::new(stdout).lines() {
        let line = line.with_context(|| format!("Failed to read {what} output"))?;
        println!("{line}");
        observe(&line);
    }
    child
        .wait()
        .with_context(|| format!("Failed to wait for {what}"))
}

pub(crate) fn execute_commands(commands: &[String], cwd: &Path) -> Result<()> {
    for command in commands {
        let mut cmd = Command::new("sh");
        cmd.args(["-c", command]).current_dir(cwd);
        run(&mut cmd, command)?;
    }
    Ok(())
}
