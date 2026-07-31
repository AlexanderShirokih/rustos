//! Двухпроходные QEMU-тесты: pass "kernel" и pass "userland".

use std::{
    io::{BufRead, BufReader},
    path::{Path, PathBuf},
    process::{Command, Stdio},
};

use anyhow::{Context, Result, bail};
use userland_image_tool::{BuildEnvironment, build_userland};

use crate::{
    artifacts::build_binary,
    context::{BuildContext, image_manifest_path},
};

/// Один проход QEMU-тестов: собираемый образ и ожидания от его вывода.
struct QemuPass<'a> {
    label: &'a str,
    features: Option<String>,
    image: &'a Path,
    timeout: u64,
    environment: &'a [BuildEnvironment<'a>],
    expected_seeds: Option<u64>,
}

pub(crate) fn qemu_test(timeout: u64) -> Result<()> {
    qemu_test_pass(QemuPass {
        label: "kernel",
        features: Some(String::from("kernel-tests")),
        image: Path::new("user/rootkeeper/image.toml"),
        timeout,
        environment: &[],
        expected_seeds: None,
    })?;
    qemu_test_pass(QemuPass {
        label: "userland",
        features: None,
        image: Path::new("user/testrunner/image.toml"),
        timeout,
        environment: &[],
        expected_seeds: None,
    })?;
    Ok(())
}

pub(crate) fn qemu_stress(seed: u64, seeds: u64, steps: u64, timeout: u64) -> Result<()> {
    let seed_count = seeds;
    let seed = seed.to_string();
    let seeds = seeds.to_string();
    let steps = steps.to_string();
    let environment = [
        BuildEnvironment {
            name: "RUSTOS_TEST_PROFILE",
            value: "stress",
        },
        BuildEnvironment {
            name: "RUSTOS_STRESS_SEED",
            value: &seed,
        },
        BuildEnvironment {
            name: "RUSTOS_STRESS_SEEDS",
            value: &seeds,
        },
        BuildEnvironment {
            name: "RUSTOS_STRESS_STEPS",
            value: &steps,
        },
    ];
    qemu_test_pass(QemuPass {
        label: "stress",
        features: None,
        image: Path::new("user/testrunner/image.toml"),
        timeout,
        environment: &environment,
        expected_seeds: Some(seed_count),
    })
}

fn qemu_test_pass(pass: QemuPass<'_>) -> Result<()> {
    let QemuPass {
        label,
        features,
        image,
        timeout,
        environment,
        expected_seeds,
    } = pass;
    println!("=== qemu-test pass: {label} ===");

    let spec_path = PathBuf::from("devices/spec/qemu-aarch64-test.yaml");
    let ctx = BuildContext::new(spec_path, features)?;

    let manifest_path = image_manifest_path(&ctx.project_root, image)?;
    build_userland(
        &ctx.project_root,
        &manifest_path,
        &ctx.build_dir,
        environment,
    )?;
    build_binary(&ctx)?;

    let qemu_cmd = ctx
        .spec
        .run
        .first()
        .context("No run commands in qemu-aarch64-test.yaml")?;

    let full_cmd = format!("timeout {timeout} {qemu_cmd}");
    let mut child = Command::new("sh")
        .args(["-c", &full_cmd])
        .current_dir(&ctx.project_root)
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::inherit())
        .spawn()
        .context("Failed to launch QEMU")?;

    let stdout = child.stdout.take().context("QEMU stdout not captured")?;
    let mut markers = TestMarkers::default();
    for line in BufReader::new(stdout).lines() {
        let line = line.context("Failed to read QEMU output")?;
        println!("{line}");
        markers.observe(&line);
    }

    let status = child.wait().context("Failed to wait for QEMU")?;
    if !status.success() {
        let code = status.code().unwrap_or(1);
        if code == 124 {
            bail!("QEMU pass '{label}' timed out after {timeout}s");
        }
        bail!("QEMU pass '{label}' exited with code {code}");
    }

    markers.verify(label, expected_seeds)
}

/// Счётчики `[TEST-*]`-маркеров прогона.
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
        if let Some(expected) = expected_seeds {
            let expected = usize::try_from(expected).unwrap_or(usize::MAX);
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
