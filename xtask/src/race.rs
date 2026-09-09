//! Host race-тесты production-примитивов под ThreadSanitizer.
//!
//! Состав прогона команда выводит из дерева. В прогон входят пакеты корневого workspace, в
//! исходниках которых есть `#[test]`, имя которого начинается на `race_`. Полноту прогона
//! подтверждает сверка имён найденных тестов с именами отчитавшихся. Счёта прошедших для этого
//! не хватает. Выпавший из прогона пакет оставляет счёт остальных ненулевым.
//!
//! Имя теста в такой сверке служит его тождеством. Повторное имя в дереве команда называет
//! отказом. Отчёт cargo содержит путь модуля без имени пакета, и сверка различает тесты по
//! последнему сегменту этого пути.

use std::{
    collections::{BTreeMap, BTreeSet},
    process::Command,
};

use anyhow::{Context, Result, bail};

use crate::{
    context::project_root,
    layers::{WorkspacePackage, workspace_packages},
    shell,
    sources::rust_sources,
};

const TEST_PREFIX: &str = "race_";

type Found = BTreeMap<String, BTreeSet<String>>;

#[derive(Debug)]
struct Selection {
    packages: Vec<String>,
    tests: BTreeSet<String>,
}

pub(crate) fn run() -> Result<()> {
    let project_root = project_root();
    let packages = workspace_packages(&project_root)?;
    let found = discover(&packages)?;
    let selection = select(&found, &host_unreachable(&packages))?;
    println!(
        "race: {} tests in {}",
        selection.tests.len(),
        selection.packages.join(", ")
    );

    let target = host_target()?;
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
    for package in &selection.packages {
        command.args(["-p", package]);
    }
    command.args([TEST_PREFIX, "--", "--test-threads=1"]);
    command
        .current_dir(project_root)
        .env(rustflags_key, "-Zsanitizer=thread")
        .env("CARGO_PROFILE_DEV_PANIC", "unwind");

    let mut reported = BTreeSet::new();
    let status = shell::run_streaming(&mut command, "cargo test", |line| {
        if let Some(name) = passed_test(line) {
            reported.insert(name.to_owned());
        }
    })?;
    if !status.success() {
        bail!("Race tests failed with {status}");
    }
    let missing = missing(&selection.tests, &reported);
    if !missing.is_empty() {
        bail!(
            "Race tests found in the tree but absent from the run: {}",
            missing.join(", ")
        );
    }
    println!(
        "race: {} tests passed under ThreadSanitizer",
        selection.tests.len()
    );
    Ok(())
}

fn discover(packages: &[WorkspacePackage]) -> Result<Found> {
    let mut found = Found::new();
    let mut occurrences = Vec::new();
    for package in packages {
        let mut tests = BTreeSet::new();
        for source in rust_sources(&package.dir)? {
            for test in race_tests(&source) {
                occurrences.push(test.to_owned());
                tests.insert(test.to_owned());
            }
        }
        if !tests.is_empty() {
            found.insert(package.name.clone(), tests);
        }
    }
    let repeated = repeated(&occurrences);
    if !repeated.is_empty() {
        bail!(
            "Race test names are the identity of the run and repeat in the tree: {}",
            repeated.join(", ")
        );
    }
    Ok(found)
}

fn repeated(occurrences: &[String]) -> Vec<&str> {
    let mut counts: BTreeMap<&str, usize> = BTreeMap::new();
    for name in occurrences {
        *counts.entry(name.as_str()).or_default() += 1;
    }
    counts
        .into_iter()
        .filter(|(_, count)| *count > 1)
        .map(|(name, _)| name)
        .collect()
}

fn host_unreachable(packages: &[WorkspacePackage]) -> BTreeSet<String> {
    packages
        .iter()
        .filter(|package| package.forced_target.is_some())
        .map(|package| package.name.clone())
        .collect()
}

fn select(found: &Found, host_unreachable: &BTreeSet<String>) -> Result<Selection> {
    let mut selection = Selection {
        packages: Vec::new(),
        tests: BTreeSet::new(),
    };
    for (package, tests) in found {
        if host_unreachable.contains(package) {
            let names: Vec<&str> = tests.iter().map(String::as_str).collect();
            bail!(
                "Race tests {} live in `{package}`, which builds only for its target architecture",
                names.join(", ")
            );
        }
        selection.packages.push(package.clone());
        selection.tests.extend(tests.iter().cloned());
    }
    if selection.tests.is_empty() {
        bail!("No test carries the `{TEST_PREFIX}` prefix - the race gate selected nothing");
    }
    Ok(selection)
}

fn race_tests(source: &str) -> Vec<&str> {
    let mut tests = Vec::new();
    let mut is_test = false;
    for line in source.lines().map(str::trim) {
        if line.is_empty() || line.starts_with("//") {
            continue;
        }
        if line.starts_with("#[") {
            // Одного имени для признака мало. Поимённая сверка делает лишнюю находку отказом,
            // и якорем служит атрибут. Накопление держит признак через другие атрибуты.
            is_test |= line == "#[test]";
            continue;
        }
        if is_test
            && let Some(name) = declared_fn(line)
            && name.starts_with(TEST_PREFIX)
        {
            tests.push(name);
        }
        is_test = false;
    }
    tests
}

fn declared_fn(line: &str) -> Option<&str> {
    let rest = after_visibility(line).strip_prefix("fn ")?;
    let end = rest.find(|ch: char| !ch.is_alphanumeric() && ch != '_')?;
    rest.get(..end)
}

fn after_visibility(line: &str) -> &str {
    let Some(rest) = line.strip_prefix("pub") else {
        return line;
    };
    let rest = match rest.strip_prefix('(') {
        Some(scope) => match scope.split_once(')') {
            Some((_, tail)) => tail,
            None => return line,
        },
        None => rest,
    };
    rest.trim_start()
}

fn passed_test(line: &str) -> Option<&str> {
    let path = line.strip_prefix("test ")?.strip_suffix(" ... ok")?;
    path.rsplit("::").next()
}

fn missing<'a>(expected: &'a BTreeSet<String>, reported: &BTreeSet<String>) -> Vec<&'a str> {
    expected
        .iter()
        .filter(|name| !reported.contains(*name))
        .map(String::as_str)
        .collect()
}

pub(crate) fn host_target() -> Result<String> {
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

    fn found(entries: &[(&str, &[&str])]) -> Found {
        entries
            .iter()
            .map(|(package, tests)| {
                (
                    (*package).to_owned(),
                    tests.iter().map(|test| (*test).to_owned()).collect(),
                )
            })
            .collect()
    }

    fn names(names: &[&str]) -> BTreeSet<String> {
        names.iter().map(|name| (*name).to_owned()).collect()
    }

    #[test]
    fn a_test_declaration_with_the_prefix_is_a_finding() {
        let source = "#[test]\nfn race_frames_are_never_handed_out_twice() {\n}\n";
        assert_eq!(
            race_tests(source),
            ["race_frames_are_never_handed_out_twice"]
        );
    }

    #[test]
    fn an_attribute_between_the_test_and_its_declaration_keeps_the_finding() {
        let source = "#[test]\n#[expect(clippy::too_many_lines)]\nfn race_rollover() {\n}\n";
        assert_eq!(race_tests(source), ["race_rollover"]);
    }

    #[test]
    fn a_visibility_modifier_before_the_declaration_keeps_the_finding() {
        assert_eq!(
            race_tests("#[test]\npub fn race_park() {\n}\n"),
            ["race_park"]
        );
        assert_eq!(
            race_tests("#[test]\npub(crate) fn race_park() {\n}\n"),
            ["race_park"]
        );
        assert_eq!(
            race_tests("#[test]\npub(in crate::spsc) fn race_park() {\n}\n"),
            ["race_park"]
        );
    }

    #[test]
    fn a_name_outside_a_test_declaration_is_not_a_finding() {
        assert!(race_tests("fn race_helper() {\n}\n").is_empty());
        assert!(race_tests("// fn race_in_a_comment()\n").is_empty());
        assert!(race_tests("#[test]\nfn ordinary_test() {\n}\n").is_empty());
    }

    #[test]
    fn the_selection_takes_the_packages_of_the_findings() {
        let selection = select(
            &found(&[
                ("collections", &["race_park"]),
                ("memory", &["race_frames"]),
            ]),
            &BTreeSet::new(),
        )
        .unwrap();
        assert_eq!(selection.packages, ["collections", "memory"]);
        assert_eq!(selection.tests, names(&["race_frames", "race_park"]));
    }

    #[test]
    fn a_finding_in_a_package_outside_the_host_is_refused() {
        let error = select(
            &found(&[("hal-aarch64", &["race_rollover"])]),
            &names(&["hal-aarch64"]),
        )
        .unwrap_err()
        .to_string();
        assert!(error.contains("race_rollover"), "{error}");
        assert!(error.contains("hal-aarch64"), "{error}");
    }

    #[test]
    fn a_selection_without_findings_is_refused() {
        assert!(select(&Found::new(), &BTreeSet::new()).is_err());
    }

    #[test]
    fn the_report_line_of_a_passed_test_yields_its_name() {
        assert_eq!(
            passed_test("test spsc_ring::tests::race_park_protocol ... ok"),
            Some("race_park_protocol")
        );
        assert_eq!(passed_test("test race_frames ... ok"), Some("race_frames"));
    }

    #[test]
    fn a_line_that_is_not_a_passed_test_yields_nothing() {
        assert_eq!(passed_test("running 12 tests"), None);
        assert_eq!(passed_test("test race_frames ... FAILED"), None);
        assert_eq!(
            passed_test("test result: ok. 12 passed; 0 failed; 0 ignored"),
            None
        );
    }

    #[test]
    fn a_name_met_twice_in_the_tree_is_named_repeated() {
        let occurrences: Vec<String> = ["race_park", "race_frames", "race_park"]
            .iter()
            .map(|name| (*name).to_owned())
            .collect();
        assert_eq!(repeated(&occurrences), ["race_park"]);
        assert!(repeated(&occurrences[..2]).is_empty());
    }

    #[test]
    fn a_test_that_did_not_report_is_named_missing() {
        let expected = names(&["race_frames", "race_park"]);
        assert_eq!(missing(&expected, &names(&["race_park"])), ["race_frames"]);
        assert!(missing(&expected, &names(&["race_frames", "race_park"])).is_empty());
    }
}
