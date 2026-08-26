//! Поиск дублирующихся функций: прогон анализатора, разбор его отчёта и сверка находок с
//! записью принятых пар.
//!
//! Находка опознаётся по форме строки, а счёт разобранных пар сверяется с итогом отчёта: отчёт
//! сменившейся формы обязан ронять прогон, не обнулять его молча.

use std::{
    collections::BTreeMap,
    fs,
    path::Path,
    process::{Command, Stdio},
};

use anyhow::{Context, Result, bail};
use clap::Args;
use serde::Deserialize;

use crate::{context::project_root, shell};

const ANALYZER: &str = "similarity-rs";
const CONFIG_FILE: &str = "similarity.toml";
const BASELINE_FILE: &str = "similarity-baseline.txt";
const SEPARATOR: &str = " <-> ";

#[derive(Args)]
pub(crate) struct SimilarityArgs {
    /// Regenerate the baseline snapshot from the current tree
    #[arg(long)]
    write_baseline: bool,
    /// Report pairs accepted by the baseline as well
    #[arg(long, conflicts_with = "write_baseline")]
    no_baseline: bool,
    /// Fail when the baseline accepts more pairs than it does at this revision
    #[arg(long, value_name = "REV", conflicts_with = "write_baseline")]
    base: Option<String>,
}

#[derive(Deserialize)]
struct Config {
    version: String,
    paths: Vec<String>,
    min_lines: u32,
    threshold: f64,
}

#[derive(Debug)]
struct Pair {
    key: String,
    report_line: String,
}

struct Side {
    path: String,
    symbol: String,
}

struct Growth {
    path: String,
    accepted: usize,
    was: usize,
}

pub(crate) fn run(args: SimilarityArgs) -> Result<()> {
    let SimilarityArgs {
        write_baseline,
        no_baseline,
        base,
    } = args;

    let project_root = project_root();
    let config = read_config(&project_root)?;
    check_analyzer_version(&config)?;

    let pairs = parse_report(&analyze(&project_root, &config)?)?;
    let baseline_path = project_root.join(BASELINE_FILE);

    if write_baseline {
        let mut content = String::new();
        for pair in &pairs {
            content.push_str(&pair.key);
            content.push('\n');
        }
        fs::write(&baseline_path, content)
            .with_context(|| format!("Failed to write {}", baseline_path.display()))?;
        println!(
            "similarity: {} accepted pairs written to {BASELINE_FILE}",
            pairs.len()
        );
        return Ok(());
    }

    let baseline = read_baseline_file(&baseline_path)?;
    let accepted = count_keys(&baseline)?;
    let unaccepted = select_unaccepted(&pairs, &accepted);

    if no_baseline {
        for pair in &pairs {
            println!("{}", pair.report_line);
        }
    } else {
        for pair in &unaccepted {
            println!("{}", pair.report_line);
        }
    }

    if !unaccepted.is_empty() {
        let noun = if unaccepted.len() == 1 {
            "pair"
        } else {
            "pairs"
        };
        bail!(
            "{} duplicate {noun} outside {BASELINE_FILE}, \
             accept them with: cargo xtask similarity --write-baseline",
            unaccepted.len()
        );
    }

    if let Some(base) = &base {
        match baseline_at(&project_root, base)? {
            None => println!("similarity: {base} carries no {BASELINE_FILE}, nothing to compare"),
            Some(recorded) => {
                let grown = select_grown(&recorded, &baseline)?;
                for growth in &grown {
                    println!(
                        "{}: {} accepted pairs, was {}",
                        growth.path, growth.accepted, growth.was
                    );
                }
                if !grown.is_empty() {
                    bail!("{BASELINE_FILE} grew against {base}, remove the duplication instead");
                }
            }
        }
    }

    println!(
        "similarity: {} pairs found, all accepted by {BASELINE_FILE}",
        pairs.len()
    );
    Ok(())
}

fn read_config(project_root: &Path) -> Result<Config> {
    let path = project_root.join(CONFIG_FILE);
    let text =
        fs::read_to_string(&path).with_context(|| format!("Failed to read {}", path.display()))?;
    toml::from_str(&text).with_context(|| format!("Failed to parse {}", path.display()))
}

fn check_analyzer_version(config: &Config) -> Result<()> {
    let expected = format!("{ANALYZER} {}", config.version);
    let install = format!(
        "cargo install {ANALYZER} --locked --version {}",
        config.version
    );

    let Ok(output) = Command::new(ANALYZER).arg("--version").output() else {
        bail!("{ANALYZER} is not available, install it with: {install}");
    };
    if !output.status.success() {
        bail!("{ANALYZER} --version failed with {}", output.status);
    }

    let reported = String::from_utf8(output.stdout)
        .with_context(|| format!("{ANALYZER} --version returned non-UTF-8 output"))?;
    let reported = reported.trim();
    if reported != expected {
        bail!("{expected} is required, found '{reported}', install it with: {install}");
    }
    Ok(())
}

fn analyze(project_root: &Path, config: &Config) -> Result<String> {
    let mut command = Command::new(ANALYZER);
    command
        .args([
            "--skip-test",
            "--min-lines",
            &config.min_lines.to_string(),
            "--threshold",
            &config.threshold.to_string(),
        ])
        .args(&config.paths)
        .current_dir(project_root)
        .stdin(Stdio::null())
        .stderr(Stdio::inherit());
    shell::output(&mut command, ANALYZER)
}

fn parse_report(report: &str) -> Result<Vec<Pair>> {
    const TOTAL: &str = "Total duplicate pairs found: ";
    const NOTHING: &str = "No duplicate functions found!";
    const CLASSES: &str = "  Classes: ";

    let mut pairs = Vec::new();
    let mut announced = None;
    for line in report.lines() {
        if let Some(total) = line.strip_prefix(TOTAL) {
            announced = Some(
                total
                    .trim()
                    .parse::<usize>()
                    .with_context(|| format!("Unreadable {ANALYZER} total: {line}"))?,
            );
        } else if line == NOTHING {
            announced = Some(0);
        } else if line.contains(SEPARATOR) && !line.starts_with(CLASSES) {
            let Some(body) = line.strip_prefix("  ") else {
                bail!("Unreadable {ANALYZER} report line: {line}");
            };
            pairs.push(parse_pair(body)?);
        }
    }

    // Итог анализатора держит разбор честным: строку находки, которую разбор
    // перестал узнавать, видно расхождением счёта.
    let Some(announced) = announced else {
        bail!("{ANALYZER} report carries no '{TOTAL}' line");
    };
    if announced != pairs.len() {
        bail!(
            "{ANALYZER} announced {announced} pairs, the report gave {}",
            pairs.len()
        );
    }

    pairs.sort_by(|left, right| {
        (&left.key, &left.report_line).cmp(&(&right.key, &right.report_line))
    });
    Ok(pairs)
}

fn parse_pair(body: &str) -> Result<Pair> {
    let Some((left, right)) = body.split_once(SEPARATOR) else {
        bail!("Unreadable {ANALYZER} report line: {body}");
    };
    let left = parse_side(left)?;
    let right = parse_side(right)?;

    // Ключ не зависит от того, какую сторону анализатор напечатал первой.
    let key = if (&left.path, &left.symbol) <= (&right.path, &right.symbol) {
        format!(
            "{}|{}|{}|{}",
            left.path, left.symbol, right.path, right.symbol
        )
    } else {
        format!(
            "{}|{}|{}|{}",
            right.path, right.symbol, left.path, left.symbol
        )
    };

    Ok(Pair {
        key,
        report_line: body.to_owned(),
    })
}

fn parse_side(side: &str) -> Result<Side> {
    let mut words = side.split_whitespace();
    let (Some(location), Some(kind), Some(name), None) =
        (words.next(), words.next(), words.next(), words.next())
    else {
        bail!("Unreadable {ANALYZER} finding side: {side}");
    };
    let Some((path, lines)) = location.rsplit_once(':') else {
        bail!("Finding side carries no line range: {side}");
    };
    if !lines
        .split_once('-')
        .is_some_and(|(first, last)| is_number(first) && is_number(last))
    {
        bail!("Finding side carries no line range: {side}");
    }
    Ok(Side {
        path: path.to_owned(),
        symbol: format!("{kind} {name}"),
    })
}

fn is_number(text: &str) -> bool {
    !text.is_empty() && text.chars().all(|symbol| symbol.is_ascii_digit())
}

fn read_baseline_file(path: &Path) -> Result<String> {
    if !path.exists() {
        return Ok(String::new());
    }
    fs::read_to_string(path).with_context(|| format!("Failed to read {}", path.display()))
}

fn baseline_at(project_root: &Path, revision: &str) -> Result<Option<String>> {
    let output = Command::new("git")
        .args(["show", &format!("{revision}:{BASELINE_FILE}")])
        .current_dir(project_root)
        .stdin(Stdio::null())
        .stderr(Stdio::null())
        .output()
        .context("Failed to run git show")?;
    if !output.status.success() {
        // Ревизия старше самой записи: сверять не с чем.
        return Ok(None);
    }
    String::from_utf8(output.stdout)
        .context("git show returned non-UTF-8 output")
        .map(Some)
}

fn count_keys(baseline: &str) -> Result<BTreeMap<String, usize>> {
    let mut accepted = BTreeMap::new();
    for line in parsed_lines(baseline)? {
        *accepted.entry(line.to_owned()).or_default() += 1;
    }
    Ok(accepted)
}

fn count_paths(baseline: &str) -> Result<BTreeMap<String, usize>> {
    let mut accepted = BTreeMap::new();
    for line in parsed_lines(baseline)? {
        let path = line.split('|').next().unwrap_or(line);
        *accepted.entry(path.to_owned()).or_default() += 1;
    }
    Ok(accepted)
}

fn parsed_lines(baseline: &str) -> Result<Vec<&str>> {
    let mut lines = Vec::new();
    for line in baseline.lines() {
        if line.trim().is_empty() {
            continue;
        }
        if line.split('|').filter(|field| !field.is_empty()).count() != 4 {
            bail!("Malformed {BASELINE_FILE} line: {line}");
        }
        lines.push(line);
    }
    Ok(lines)
}

fn select_unaccepted<'a>(pairs: &'a [Pair], accepted: &BTreeMap<String, usize>) -> Vec<&'a Pair> {
    let mut remaining = accepted.clone();
    pairs
        .iter()
        .filter(|pair| match remaining.get_mut(&pair.key) {
            Some(count) if *count > 0 => {
                *count -= 1;
                false
            }
            _ => true,
        })
        .collect()
}

fn select_grown(base: &str, head: &str) -> Result<Vec<Growth>> {
    let base = count_paths(base)?;
    Ok(count_paths(head)?
        .into_iter()
        .filter_map(|(path, accepted)| {
            let was = base.get(&path).copied().unwrap_or(0);
            (accepted > was).then_some(Growth {
                path,
                accepted,
                was,
            })
        })
        .collect())
}

#[cfg(test)]
mod tests {
    use super::*;

    const REPORT: &str = "\
Analyzing Rust code similarity...

=== Function Similarity ===
Checking 2 files for duplicates...

Duplicates in alpha/src/one.rs:
------------------------------------------------------------
  alpha/src/one.rs:10-24 method read <-> alpha/src/one.rs:26-40 method write
  Similarity: 93.37%
  Classes: Store <-> Store
  beta/src/two.rs:5-19 function send <-> beta/src/two.rs:21-35 function recv
  Similarity: 95.56%

Total duplicate pairs found: 2
";

    const ACCEPTED: &str = "\
alpha/src/one.rs|method read|alpha/src/one.rs|method write
beta/src/two.rs|function recv|beta/src/two.rs|function send
";

    #[test]
    fn parses_findings_with_and_without_classes() {
        let pairs = parse_report(REPORT).expect("report parses");
        let keys: Vec<&str> = pairs.iter().map(|pair| pair.key.as_str()).collect();
        assert_eq!(
            keys,
            [
                "alpha/src/one.rs|method read|alpha/src/one.rs|method write",
                "beta/src/two.rs|function recv|beta/src/two.rs|function send",
            ]
        );
    }

    #[test]
    fn a_report_shorter_than_its_own_total_fails() {
        let report = REPORT.replace(
            "Total duplicate pairs found: 2",
            "Total duplicate pairs found: 3",
        );
        let error = parse_report(&report).expect_err("count mismatch fails");
        assert!(error.to_string().contains("announced 3"));
    }

    #[test]
    fn a_report_without_a_total_fails() {
        let report = REPORT.replace("Total duplicate pairs found: 2\n", "");
        assert!(parse_report(&report).is_err());
    }

    #[test]
    fn a_report_of_no_duplicates_yields_no_pair() {
        let report = "No duplicate functions found!\n";
        assert!(parse_report(report).expect("report parses").is_empty());
    }

    #[test]
    fn finding_without_line_range_fails_the_report() {
        let report =
            "  a.rs function one <-> b.rs:1-9 function two\nTotal duplicate pairs found: 1\n";
        assert!(parse_report(report).is_err());
    }

    #[test]
    fn key_ignores_the_order_of_sides() {
        let direct = parse_pair("a.rs:1-9 function one <-> b.rs:2-8 method two")
            .expect("finding parses")
            .key;
        let reversed = parse_pair("b.rs:2-8 method two <-> a.rs:1-9 function one")
            .expect("finding parses")
            .key;
        assert_eq!(direct, reversed);
    }

    #[test]
    fn accepted_pair_is_silent_and_a_surplus_is_reported() {
        let pairs = parse_report(REPORT).expect("report parses");
        let accepted = count_keys(ACCEPTED).expect("baseline parses");
        assert!(select_unaccepted(&pairs, &accepted).is_empty());

        let accepted =
            count_keys(ACCEPTED.lines().next().expect("first line")).expect("baseline parses");
        let unaccepted = select_unaccepted(&pairs, &accepted);
        assert_eq!(unaccepted.len(), 1);
        assert_eq!(unaccepted[0].key, pairs[1].key);
    }

    #[test]
    fn accepted_pair_missing_from_the_tree_is_ignored() {
        let pairs = parse_report(REPORT).expect("report parses");
        let baseline = format!("{ACCEPTED}gone.rs|function gone|gone.rs|function also_gone\n");
        let accepted = count_keys(&baseline).expect("baseline parses");
        assert!(select_unaccepted(&pairs, &accepted).is_empty());
    }

    #[test]
    fn repeated_key_is_accepted_by_count() {
        let line = "  a.rs:1-9 method same <-> a.rs:20-28 method same";
        let report = format!("{line}\n{line}\n{line}\nTotal duplicate pairs found: 3\n");
        let pairs = parse_report(&report).expect("report parses");
        let baseline = format!("{}\n{}\n", pairs[0].key, pairs[0].key);
        let accepted = count_keys(&baseline).expect("baseline parses");
        assert_eq!(select_unaccepted(&pairs, &accepted).len(), 1);
    }

    #[test]
    fn malformed_baseline_line_fails_the_read() {
        let error = count_keys("a.rs|function one|b.rs\n").expect_err("malformed line fails");
        assert!(error.to_string().contains("a.rs|function one|b.rs"));
    }

    #[test]
    fn growth_of_a_known_path_is_reported() {
        let head = format!("{ACCEPTED}alpha/src/one.rs|method read|alpha/src/one.rs|method zero\n");
        let grown = select_grown(ACCEPTED, &head).expect("baselines parse");
        assert_eq!(grown.len(), 1);
        assert_eq!(grown[0].path, "alpha/src/one.rs");
        assert_eq!((grown[0].accepted, grown[0].was), (2, 1));
    }

    #[test]
    fn growth_of_an_unseen_path_is_reported() {
        let head =
            format!("{ACCEPTED}gamma/src/new.rs|function one|gamma/src/new.rs|function two\n");
        let grown = select_grown(ACCEPTED, &head).expect("baselines parse");
        assert_eq!(grown.len(), 1);
        assert_eq!(grown[0].path, "gamma/src/new.rs");
        assert_eq!((grown[0].accepted, grown[0].was), (1, 0));
    }

    #[test]
    fn unchanged_and_shrunk_baselines_do_not_grow() {
        assert!(
            select_grown(ACCEPTED, ACCEPTED)
                .expect("baselines parse")
                .is_empty()
        );
        let head = ACCEPTED.lines().next().expect("first line");
        assert!(
            select_grown(ACCEPTED, head)
                .expect("baselines parse")
                .is_empty()
        );
    }

    #[test]
    fn an_empty_record_accepts_nothing() {
        let grown = select_grown("", ACCEPTED).expect("baselines parse");
        assert_eq!(grown.len(), 2);
    }
}
