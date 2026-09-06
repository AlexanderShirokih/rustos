//! Форматирование дерева целиком. `cargo fmt --all` видит только членов своего workspace, а
//! часть дерева живёт вне корневого: список отделённых берётся из `workspace.exclude`
//! корневого манифеста, второго реестра в дереве нет.

use std::{
    io::{Write, stderr, stdout},
    path::Path,
    process::{Command, Output},
    thread,
};

use anyhow::{Context, Result, bail};

use crate::{context::project_root, layers::detached_workspaces};

pub(crate) fn run(check: bool) -> Result<()> {
    let root = project_root();
    let mut manifests = vec![root.join("Cargo.toml")];
    for workspace in detached_workspaces(&root)? {
        manifests.push(workspace.join("Cargo.toml"));
    }

    // Форматирование workspace выполняется параллельно, вывод накапливается по каждому
    // манифесту и печатается в порядке списка.
    let outputs: Vec<Result<Output>> = thread::scope(|scope| {
        let handles: Vec<_> = manifests
            .iter()
            .map(|manifest| scope.spawn(move || format_workspace(manifest, check)))
            .collect();
        handles.into_iter().map(join).collect()
    });

    let mut unformatted = Vec::new();
    for (manifest, output) in manifests.iter().zip(outputs) {
        let output = output?;
        report(&output)?;
        // Под `--check` ненулевой код отвечает и неотформатированному дереву, и отказу
        // самого rustfmt. Без него ненулевой код означает отказ.
        if !check && !output.status.success() {
            bail!(
                "cargo fmt over {} failed with {}",
                manifest.display(),
                output.status
            );
        }
        if !output.status.success() {
            unformatted.push(manifest.clone());
        }
    }

    if !unformatted.is_empty() {
        for manifest in &unformatted {
            eprintln!("fmt: {} needs formatting", manifest.display());
        }
        bail!(
            "fmt: {} of {} workspaces need formatting; run `cargo xtask fmt`",
            unformatted.len(),
            manifests.len()
        );
    }
    println!("fmt: OK ({} workspaces)", manifests.len());
    Ok(())
}

fn format_workspace(manifest: &Path, check: bool) -> Result<Output> {
    let mut command = Command::new("cargo");
    command
        .arg("fmt")
        .arg("--manifest-path")
        .arg(manifest)
        .arg("--all");
    if check {
        command.arg("--check");
    }
    command
        .output()
        .with_context(|| format!("Failed to run cargo fmt for {}", manifest.display()))
}

fn report(output: &Output) -> Result<()> {
    let mut out = stdout().lock();
    out.write_all(&output.stdout)?;
    out.flush().context("Failed to print the rustfmt report")?;
    let mut err = stderr().lock();
    err.write_all(&output.stderr)?;
    err.flush().context("Failed to print the rustfmt report")
}

fn join<T>(handle: thread::ScopedJoinHandle<'_, T>) -> T {
    match handle.join() {
        Ok(value) => value,
        Err(panic) => std::panic::resume_unwind(panic),
    }
}
