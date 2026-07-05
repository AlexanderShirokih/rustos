//! Утилиты запуска внешних команд.

use std::{
    path::Path,
    process::{Command, ExitStatus, Stdio},
};

use anyhow::{Context, Result, bail};

pub(crate) fn run_cmd(cmd: &mut Command) -> Result<ExitStatus> {
    let status = cmd.status().context("Failed to execute command")?;
    if !status.success() {
        bail!("Command failed with {status}");
    }
    Ok(status)
}

pub(crate) fn run_shell(command: &str, cwd: &Path) -> Result<()> {
    let mut child = Command::new("sh")
        .args(["-c", command])
        .current_dir(cwd)
        .stdin(Stdio::inherit())
        .stdout(Stdio::inherit())
        .stderr(Stdio::inherit())
        .spawn()
        .context("Failed to execute shell command")?;

    let status = child.wait().context("Failed to wait for command")?;

    if !status.success() {
        bail!("Command failed: {command}");
    }
    Ok(())
}

pub(crate) fn execute_commands(commands: &[String], cwd: &Path) -> Result<()> {
    for cmd in commands {
        run_shell(cmd, cwd)?;
    }
    Ok(())
}
