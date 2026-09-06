//! Прогон основных проверок дерева одной командой: набор проверок, их команды и очереди
//! заданы конфигурацией `checks.toml`.

use std::{
    io::{Write, stdout},
    path::Path,
    process::{Command, ExitStatus, Output},
    sync::{
        Mutex, PoisonError,
        atomic::{AtomicBool, Ordering},
    },
    thread,
    time::{Duration, Instant},
};

use anyhow::{Context, Result, bail};
use clap::Args;
use serde::Deserialize;

use crate::context::project_root;

const CONFIG: &str = "checks.toml";
const DEFAULT_LANE: &str = "cargo";

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
    /// Start no further check once one has failed
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
    #[serde(default = "default_lane")]
    lane: String,
}

fn default_lane() -> String {
    DEFAULT_LANE.to_owned()
}

struct Outcome<'a> {
    index: usize,
    name: &'a str,
    status: ExitStatus,
    elapsed: Duration,
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

    let lanes = lanes(&selected);
    let outcomes = if lanes.len() < 2 {
        serial(&selected, &root, args.fail_fast)?
    } else {
        parallel(&lanes, &root, args.fail_fast)?
    };

    let failed: Vec<(&str, ExitStatus, Duration)> = outcomes
        .iter()
        .filter(|outcome| !outcome.status.success())
        .map(|outcome| (outcome.name, outcome.status, outcome.elapsed))
        .collect();

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

fn lanes<'a>(selected: &[&'a Check]) -> Vec<Vec<(usize, &'a Check)>> {
    let mut lanes: Vec<(&str, Vec<(usize, &Check)>)> = Vec::new();
    for (index, check) in selected.iter().enumerate() {
        if let Some((_, lane)) = lanes.iter_mut().find(|(name, _)| *name == check.lane) {
            lane.push((index, check));
        } else {
            lanes.push((&check.lane, vec![(index, check)]));
        }
    }
    lanes.into_iter().map(|(_, lane)| lane).collect()
}

fn serial<'a>(selected: &[&'a Check], root: &Path, fail_fast: bool) -> Result<Vec<Outcome<'a>>> {
    let mut outcomes = Vec::new();
    for (index, check) in selected.iter().enumerate() {
        println!("== {} ==", check.name);
        println!("{}", check.run.trim());
        let started = Instant::now();
        let status = execute(&check.run, root)
            .with_context(|| format!("Failed to run the '{}' check", check.name))?;
        let elapsed = started.elapsed();
        println!(
            "-- {} {} in {}s",
            check.name,
            verdict(status),
            elapsed.as_secs()
        );
        outcomes.push(Outcome {
            index,
            name: &check.name,
            status,
            elapsed,
        });
        if fail_fast && !status.success() {
            break;
        }
    }
    Ok(outcomes)
}

fn parallel<'a>(
    lanes: &[Vec<(usize, &'a Check)>],
    root: &Path,
    fail_fast: bool,
) -> Result<Vec<Outcome<'a>>> {
    // Строки соседних очередей перемешиваются на общем терминале, поэтому вывод проверки
    // копится и печатается целым блоком, когда проверка кончилась.
    let total: usize = lanes.iter().map(Vec::len).sum();
    println!(
        "check: {total} checks in {} lanes, the output of a check is printed when it ends",
        lanes.len()
    );

    let stop = AtomicBool::new(false);
    let console = Mutex::new(());
    let lane_results: Vec<Result<Vec<Outcome<'a>>>> = thread::scope(|scope| {
        let handles: Vec<_> = lanes
            .iter()
            .map(|lane| {
                let stop = &stop;
                let console = &console;
                scope.spawn(move || lane_run(lane, root, fail_fast, stop, console))
            })
            .collect();
        handles.into_iter().map(join).collect()
    });

    let mut outcomes = Vec::new();
    for result in lane_results {
        outcomes.extend(result?);
    }
    outcomes.sort_by_key(|outcome| outcome.index);
    Ok(outcomes)
}

fn lane_run<'a>(
    lane: &[(usize, &'a Check)],
    root: &Path,
    fail_fast: bool,
    stop: &AtomicBool,
    console: &Mutex<()>,
) -> Result<Vec<Outcome<'a>>> {
    let mut outcomes = Vec::new();
    for (index, check) in lane {
        if fail_fast && stop.load(Ordering::Relaxed) {
            break;
        }
        let started = Instant::now();
        let output = capture(&check.run, root)
            .with_context(|| format!("Failed to run the '{}' check", check.name))?;
        let elapsed = started.elapsed();
        if !output.status.success() {
            stop.store(true, Ordering::Relaxed);
        }
        let guard = console.lock().unwrap_or_else(PoisonError::into_inner);
        print_block(check, &output, elapsed)?;
        drop(guard);
        outcomes.push(Outcome {
            index: *index,
            name: &check.name,
            status: output.status,
            elapsed,
        });
        if fail_fast && !output.status.success() {
            break;
        }
    }
    Ok(outcomes)
}

fn print_block(check: &Check, output: &Output, elapsed: Duration) -> Result<()> {
    let mut out = stdout().lock();
    writeln!(out, "== {} ==", check.name)?;
    writeln!(out, "{}", check.run.trim())?;
    out.write_all(&output.stdout)?;
    writeln!(
        out,
        "-- {} {} in {}s",
        check.name,
        verdict(output.status),
        elapsed.as_secs()
    )?;
    out.flush().context("Failed to print the check report")
}

fn verdict(status: ExitStatus) -> &'static str {
    if status.success() { "ok" } else { "FAILED" }
}

fn join<T>(handle: thread::ScopedJoinHandle<'_, T>) -> T {
    match handle.join() {
        Ok(value) => value,
        Err(panic) => std::panic::resume_unwind(panic),
    }
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

fn capture(script: &str, root: &Path) -> Result<Output> {
    // `exec 2>&1` уводит поток ошибок скрипта в его же поток вывода. Два потока приходят
    // разными трубами, и порядок строк между ними теряется.
    let script = format!("exec 2>&1\n{script}");
    Command::new("bash")
        .args(["-e", "-c", &script])
        .current_dir(root)
        .output()
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
    fn the_checks_of_the_tree_fall_into_more_than_one_lane() {
        let checks = parse(include_str!("../../checks.toml")).expect("checks.toml parses");
        let selected: Vec<&Check> = checks.iter().filter(|check| !check.heavy).collect();
        assert!(lanes(&selected).len() > 1);
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
        assert_eq!(checks[0].lane, DEFAULT_LANE);
    }

    #[test]
    fn a_lane_keeps_the_order_of_declaration() {
        let text = "
[[check]]
name = 'fmt'
lane = 'tree'
run = 'true'

[[check]]
name = 'doc'
lane = 'cargo'
run = 'true'

[[check]]
name = 'style'
lane = 'tree'
run = 'true'
";
        let checks = parse(text).expect("the configuration parses");
        let selected: Vec<&Check> = checks.iter().collect();
        let lanes = lanes(&selected);
        assert_eq!(lanes.len(), 2);
        let names: Vec<&str> = lanes[0]
            .iter()
            .map(|(_, check)| check.name.as_str())
            .collect();
        assert_eq!(names, ["fmt", "style"]);
        assert_eq!(lanes[0][1].0, 2);
        assert_eq!(lanes[1].len(), 1);
    }

    #[test]
    fn a_configuration_without_checks_is_refused() {
        assert!(parse("").is_err());
    }
}
