//! Двухпроходные QEMU-тесты: pass "kernel" и pass "userland".

use std::{path::Path, process::Command};

use anyhow::{Context, Result, bail};
use userland_image_tool::BuildEnvironment;

use crate::{artifacts::build_binary, context::BuildContext, shell, userland};

pub(crate) const DEFAULT_SPEC: &str = "devices/spec/qemu-aarch64-test.yaml";

const TESTRUNNER_IMAGE: &str = "user/testrunner/image.toml";

struct QemuPass<'a> {
    label: &'a str,
    spec: &'a Path,
    features: Option<String>,
    image: &'a Path,
    timeout: u64,
    environment: &'a [BuildEnvironment<'a>],
    expected_seeds: Option<u64>,
}

pub(crate) fn qemu_test(timeout: u64, spec: &Path) -> Result<()> {
    let image = Path::new(TESTRUNNER_IMAGE);
    qemu_test_pass(QemuPass {
        label: "kernel",
        spec,
        features: Some(String::from("kernel-tests")),
        image,
        timeout,
        environment: &[],
        expected_seeds: None,
    })?;
    qemu_test_pass(QemuPass {
        label: "userland",
        spec,
        features: None,
        image,
        timeout,
        environment: &[],
        expected_seeds: None,
    })?;
    Ok(())
}

pub(crate) fn qemu_bench(timeout: u64) -> Result<()> {
    qemu_test_pass(QemuPass {
        label: "bench",
        spec: Path::new(DEFAULT_SPEC),
        features: None,
        image: Path::new(TESTRUNNER_IMAGE),
        timeout,
        environment: &[BuildEnvironment {
            name: "TEST_PROFILE",
            value: "bench",
        }],
        expected_seeds: None,
    })
}

pub(crate) fn qemu_stress(seed: u64, seeds: u64, steps: u64, timeout: u64) -> Result<()> {
    let first_seed = seed.to_string();
    let seed_count = seeds.to_string();
    let step_count = steps.to_string();
    let environment = [
        BuildEnvironment {
            name: "TEST_PROFILE",
            value: "stress",
        },
        BuildEnvironment {
            name: "STRESS_SEED",
            value: &first_seed,
        },
        BuildEnvironment {
            name: "STRESS_SEEDS",
            value: &seed_count,
        },
        BuildEnvironment {
            name: "STRESS_STEPS",
            value: &step_count,
        },
    ];
    qemu_test_pass(QemuPass {
        label: "stress",
        spec: Path::new(DEFAULT_SPEC),
        // Короткий квант режет окно между операциями и расширяет чередования.
        features: Some(String::from("short-quantum")),
        image: Path::new(TESTRUNNER_IMAGE),
        timeout,
        environment: &environment,
        expected_seeds: Some(seeds),
    })
}

fn qemu_test_pass(pass: QemuPass<'_>) -> Result<()> {
    let QemuPass {
        label,
        spec,
        features,
        image,
        timeout,
        environment,
        expected_seeds,
    } = pass;
    println!("=== qemu-test pass: {label} ===");

    let ctx = BuildContext::new(spec, features)?;

    userland::build_image(&ctx.project_root, &ctx.build_dir, image, environment, &[])?;
    build_binary(&ctx)?;

    let qemu_cmd = ctx
        .spec
        .run
        .first()
        .with_context(|| format!("No run commands in {}", spec.display()))?;

    let mut command = Command::new("sh");
    command
        .args(["-c", &format!("timeout {timeout} {qemu_cmd}")])
        .current_dir(&ctx.project_root);

    let mut markers = TestMarkers::default();
    let status = shell::run_streaming(&mut command, "QEMU", |line| markers.observe(line))?;
    if !status.success() {
        let code = status.code().unwrap_or(1);
        if code == 124 {
            bail!("QEMU pass '{label}' timed out after {timeout}s");
        }
        bail!("QEMU pass '{label}' exited with code {code}");
    }

    markers.verify(label, expected_seeds)
}

#[derive(Default)]
struct TestMarkers {
    announced: Option<usize>,
    started: usize,
    passed: usize,
    failed: usize,
    done: Option<usize>,
    stress_seeds: usize,
}

impl TestMarkers {
    fn observe(&mut self, line: &str) {
        fn suffix_count(line: &str, prefix: &str) -> Option<usize> {
            let tail = line.split(prefix).nth(1)?;
            tail.split(']').next()?.trim().parse().ok()
        }

        if line.contains("[TEST-RUN: ") {
            self.announced = suffix_count(line, "[TEST-RUN: ");
        } else if line.contains("[TEST-START: ") {
            self.started += 1;
        } else if line.contains("[TEST-PASS: ") {
            self.passed += 1;
        } else if line.contains("[TEST-FAIL") {
            self.failed += 1;
        } else if line.contains("[TEST-DONE: ") {
            self.done = suffix_count(line, "[TEST-DONE: ");
        } else if line.contains("[STRESS-PASS: ") {
            self.stress_seeds += 1;
        }
    }

    fn verify(&self, label: &str, expected_seeds: Option<u64>) -> Result<()> {
        if self.failed > 0 {
            bail!(
                "QEMU pass '{label}': {} tests reported TEST-FAIL",
                self.failed
            );
        }
        let Some(announced) = self.announced else {
            bail!("QEMU pass '{label}': TEST-RUN marker not found");
        };
        if announced == 0 {
            bail!("QEMU pass '{label}': TEST-RUN announced no tests for this profile");
        }
        if self.done != Some(announced) {
            bail!(
                "QEMU pass '{label}': TEST-DONE does not match TEST-RUN ({:?} != {announced}) - \
                 the run was cut short",
                self.done
            );
        }
        if self.started != announced || self.passed != announced {
            bail!(
                "QEMU pass '{label}': of {announced} tests {} started and {} passed - \
                 some tests were silently skipped",
                self.started,
                self.passed
            );
        }
        if let Some(seeds) = expected_seeds {
            // Диапазон seed проходит каждый stress-тест прохода, и каждый отчитывается за свой
            // seed отдельным маркером.
            let expected = usize::try_from(seeds).unwrap_or(usize::MAX) * announced;
            if self.stress_seeds != expected {
                bail!(
                    "QEMU pass '{label}': {} of {expected} stress seeds reported STRESS-PASS",
                    self.stress_seeds
                );
            }
        }
        println!("qemu-test pass '{label}': {announced} tests passed");
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn observe_all(lines: &[&str]) -> TestMarkers {
        let mut markers = TestMarkers::default();
        for line in lines {
            markers.observe(line);
        }
        markers
    }

    fn full_run(count: usize) -> Vec<String> {
        let mut lines = vec![format!("[I] [testrunner] [TEST-RUN: {count}]")];
        for index in 0..count {
            lines.push(format!("[I] [testrunner] [TEST-START: case_{index}]"));
            lines.push(format!("[I] [testrunner] [TEST-PASS: case_{index}]"));
        }
        lines.push(format!("[I] [testrunner] [TEST-DONE: {count}]"));
        lines
    }

    fn borrow(lines: &[String]) -> Vec<&str> {
        lines.iter().map(String::as_str).collect()
    }

    #[test]
    fn a_complete_run_verifies() {
        let lines = full_run(3);
        let markers = observe_all(&borrow(&lines));
        assert_eq!(markers.announced, Some(3));
        assert_eq!(markers.started, 3);
        assert_eq!(markers.passed, 3);
        assert!(markers.verify("kernel", None).is_ok());
    }

    #[test]
    fn a_run_cut_short_is_refused() {
        let mut lines = full_run(3);
        lines.pop();
        assert!(observe_all(&borrow(&lines)).verify("kernel", None).is_err());
    }

    #[test]
    fn a_silently_skipped_test_is_refused() {
        let lines = [
            "[TEST-RUN: 2]",
            "[TEST-START: case_0]",
            "[TEST-PASS: case_0]",
            "[TEST-DONE: 2]",
        ];
        assert!(observe_all(&lines).verify("kernel", None).is_err());
    }

    #[test]
    fn a_reported_failure_is_refused() {
        let mut lines = full_run(1);
        lines.insert(1, "[TEST-FAIL: case_0]".to_string());
        assert!(observe_all(&borrow(&lines)).verify("kernel", None).is_err());
    }

    #[test]
    fn a_missing_run_marker_is_refused() {
        assert!(
            observe_all(&["[TEST-PASS: case_0]"])
                .verify("kernel", None)
                .is_err()
        );
    }

    #[test]
    fn a_profile_announcing_no_tests_is_refused() {
        let lines = ["[TEST-RUN: 0]", "[TEST-DONE: 0]"];
        assert!(observe_all(&lines).verify("kernel", None).is_err());
    }

    #[test]
    fn every_seed_reports_for_every_stress_test() {
        let mut lines = full_run(2);
        for _ in 0..4 {
            lines.push("[STRESS-PASS: 7]".to_string());
        }
        let markers = observe_all(&borrow(&lines));
        assert!(markers.verify("stress", Some(2)).is_ok());
        assert!(markers.verify("stress", Some(3)).is_err());
    }
}
