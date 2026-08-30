//! Прогон основных проверок дерева одной командой: набор проверок и их команды заданы
//! конфигурацией `checks.toml`.

use std::{
    path::Path,
    process::{Command, ExitStatus},
    time::{Duration, Instant},
};

use anyhow::{Context, Result, bail};
use clap::Args;
use serde::Deserialize;

use crate::context::project_root;

const CONFIG: &str = "checks.toml";

#[derive(Args)]
pub(crate) struct CheckArgs {
    /// List the checks instead of running them
    #[arg(long)]
    list: bool,
    /// Run only the named check; repeat for several
    #[arg(long = "only", value_name = "NAME")]
    only: Vec<String>,
    /// Add the checks that need a tool beyond the ordinary build
    #[arg(long)]
    all: bool,
    /// Stop at the first failed check
    #[arg(long)]
    fail_fast: bool,
}

#[derive(Deserialize)]
struct Config {
    check: Vec<Check>,
}

#[derive(Deserialize)]
struct Check {
    name: String,
    run: String,
    #[serde(default)]
    heavy: bool,
}

pub(crate) fn run(args: &CheckArgs) -> Result<u8> {
    let root = project_root();
    let checks = load(&root.join(CONFIG))?;
    let selected = select(&checks, args)?;

    if args.list {
        for check in selected {
            println!("{}", check.name);
        }
        return Ok(0);
    }

    let mut failed: Vec<(&str, ExitStatus, Duration)> = Vec::new();
    for check in &selected {
        println!("== {} ==", check.name);
        println!("{}", check.run.trim());
        let started = Instant::now();
        let status = execute(&check.run, &root)
            .with_context(|| format!("Failed to run the '{}' check", check.name))?;
        let elapsed = started.elapsed();
        if status.success() {
            println!("-- {} ok in {}s", check.name, elapsed.as_secs());
        } else {
            println!("-- {} FAILED in {}s", check.name, elapsed.as_secs());
            failed.push((&check.name, status, elapsed));
            if args.fail_fast {
                break;
            }
        }
    }

    report(selected.len(), &failed);
    Ok(u8::from(!failed.is_empty()))
}

fn report(total: usize, failed: &[(&str, ExitStatus, Duration)]) {
    if failed.is_empty() {
        println!("check: {total} checks passed");
        return;
    }
    println!("check: {} of {total} checks failed", failed.len());
    for (name, status, elapsed) in failed {
        let code = status
            .code()
            .map_or_else(|| "signal".to_owned(), |code| code.to_string());
        println!("  {name}: exit {code} after {}s", elapsed.as_secs());
    }
}

fn load(path: &Path) -> Result<Vec<Check>> {
    let text = std::fs::read_to_string(path)
        .with_context(|| format!("Failed to read {}", path.display()))?;
    parse(&text).with_context(|| format!("Failed to read the checks of {}", path.display()))
}

fn parse(text: &str) -> Result<Vec<Check>> {
    let config: Config = toml::from_str(text).context("Failed to parse the check configuration")?;
    if config.check.is_empty() {
        bail!("The configuration declares no check");
    }
    Ok(config.check)
}

fn select<'a>(checks: &'a [Check], args: &CheckArgs) -> Result<Vec<&'a Check>> {
    if args.only.is_empty() {
        return Ok(checks
            .iter()
            .filter(|check| args.all || !check.heavy)
            .collect());
    }

    let mut selected = Vec::new();
    for name in &args.only {
        let check = checks
            .iter()
            .find(|check| &check.name == name)
            .with_context(|| {
                let known: Vec<&str> = checks.iter().map(|check| check.name.as_str()).collect();
                format!("Unknown check '{name}'; known checks: {}", known.join(", "))
            })?;
        selected.push(check);
    }
    Ok(selected)
}

fn execute(script: &str, root: &Path) -> Result<ExitStatus> {
    // Скрипт из нескольких строк идёт одним `bash -e`: переменные окружения одной строки
    // видны следующим, а первая же неудача обрывает проверку.
    Command::new("bash")
        .args(["-e", "-c", script])
        .current_dir(root)
        .status()
        .context("Failed to run bash")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_configuration_of_the_tree_parses() {
        let checks = parse(include_str!("../../checks.toml")).expect("checks.toml parses");
        assert!(checks.iter().any(|check| check.heavy));
        assert!(checks.iter().any(|check| !check.heavy));
    }

    #[test]
    fn a_check_is_ordinary_unless_it_declares_otherwise() {
        let text = "
[[check]]
name = 'fmt'
run = 'cargo xtask fmt --check'

[[check]]
name = 'qemu-test'
run = 'cargo xtask qemu-test'
heavy = true
";
        let checks = parse(text).expect("the configuration parses");
        assert_eq!(checks.len(), 2);
        assert!(!checks[0].heavy);
        assert!(checks[1].heavy);
    }

    #[test]
    fn a_configuration_without_checks_is_refused() {
        assert!(parse("").is_err());
    }
}
