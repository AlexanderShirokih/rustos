//! Двухпроходные QEMU-тесты: pass "kernel" и pass "userland".

use std::{fs, path::Path, process::Command};

use anyhow::{Context, Result, bail};
use userland_image_tool::{BuildEnvironment, ExtraFile};

use crate::{artifacts::build_binary, context::BuildContext, shell, userland};

pub(crate) const DEFAULT_SPEC: &str = "devices/spec/qemu-aarch64-test.yaml";

const TESTRUNNER_IMAGE: &str = "user/testrunner/image.toml";

const STRESS_PLAN_IMAGE_PATH: &str = "/boot/testrunner/stress-plan";

const STRESS_PLAN_SOURCE: &str = "stress-plan.txt";

struct QemuPass<'a> {
    label: &'a str,
    spec: &'a Path,
    features: Option<String>,
    image: &'a Path,
    timeout: u64,
    environment: &'a [BuildEnvironment<'a>],
    files: &'a [ExtraFile],
    expected_seeds: Option<u64>,
    expected_epochs: Option<u64>,
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
        files: &[],
        expected_seeds: None,
        expected_epochs: None,
    })?;
    qemu_test_pass(QemuPass {
        label: "userland",
        spec,
        features: None,
        image,
        timeout,
        environment: &[],
        files: &[],
        expected_seeds: None,
        expected_epochs: None,
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
        files: &[],
        expected_seeds: None,
        expected_epochs: None,
    })
}

pub(crate) fn qemu_stress(
    seed: u64,
    seeds: u64,
    steps: u64,
    epoch: u64,
    shard: u64,
    shards: u64,
    timeout: u64,
) -> Result<()> {
    if epoch == 0 {
        bail!("--epoch must be greater than zero");
    }
    let subrange = shard_subrange(seed, seeds, shard, shards)?;
    let (seed, seeds) = (subrange.first_seed, subrange.seed_count);

    let project_root = crate::context::project_root();
    let plan_source = crate::context::build_dir(&project_root)?.join(STRESS_PLAN_SOURCE);
    fs::write(&plan_source, encode_plan(seed, seeds, steps, epoch))?;
    println!(
        "=== stress plan: cargo xtask stress --seed {seed} --seeds {seeds} \
--steps {steps} --epoch {epoch} --timeout {timeout} (shard {shard} of {shards}) ==="
    );

    let files = [ExtraFile {
        path: String::from(STRESS_PLAN_IMAGE_PATH),
        source: plan_source,
    }];
    qemu_test_pass(QemuPass {
        label: "stress",
        spec: Path::new(DEFAULT_SPEC),
        // Короткий квант режет окно между операциями и расширяет чередования.
        features: Some(String::from("short-quantum")),
        image: Path::new(TESTRUNNER_IMAGE),
        timeout,
        environment: &[BuildEnvironment {
            name: "TEST_PROFILE",
            value: "stress",
        }],
        files: &files,
        expected_seeds: Some(seeds),
        expected_epochs: Some(seeds * expected_epochs(steps, epoch)),
    })
}

fn encode_plan(seed: u64, seeds: u64, steps: u64, epoch: u64) -> String {
    format!("seed={seed}\nseeds={seeds}\nsteps={steps}\nepoch={epoch}\n")
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct SeedShard {
    first_seed: u64,
    seed_count: u64,
}

fn shard_subrange(seed: u64, seeds: u64, shard: u64, shards: u64) -> Result<SeedShard> {
    if shards == 0 {
        bail!("--shards must be greater than zero");
    }
    if shard >= shards {
        bail!("--shard {shard} is out of range for {shards} shards");
    }
    // Деление нацело обязательно: потерянный хвост диапазона оставляет все
    // шарды зелёными.
    if !seeds.is_multiple_of(shards) {
        bail!("--seeds {seeds} does not divide into {shards} shards");
    }

    let seed_count = seeds / shards;
    Ok(SeedShard {
        // Прогон продвигает seed с переносом, и начало шарда считается так же.
        first_seed: seed.wrapping_add(shard * seed_count),
        seed_count,
    })
}

fn expected_epochs(steps: u64, epoch: u64) -> u64 {
    steps.div_ceil(epoch)
}

fn qemu_test_pass(pass: QemuPass<'_>) -> Result<()> {
    let QemuPass {
        label,
        spec,
        features,
        image,
        timeout,
        environment,
        files,
        expected_seeds,
        expected_epochs,
    } = pass;
    println!("=== qemu-test pass: {label} ===");

    let ctx = BuildContext::new(spec, features)?;

    userland::build_image(&ctx.project_root, &ctx.build_dir, image, environment, files)?;
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
        if let Some(refusal) = markers.expiry_refusal(label) {
            bail!(refusal);
        }
        if code == 124 {
            bail!("QEMU pass '{label}' timed out after {timeout}s");
        }
        bail!("QEMU pass '{label}' exited with code {code}");
    }

    markers.verify(label, expected_seeds, expected_epochs)
}

#[derive(Default)]
struct TestMarkers {
    announced: Option<usize>,
    started: usize,
    passed: usize,
    failed: usize,
    done: Option<usize>,
    stress_seeds: usize,
    stress_epochs: usize,
    stress_expiry: Option<String>,
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
        } else if line.contains("[STRESS-EPOCH: ") {
            self.stress_epochs += 1;
        } else if line.contains("[STRESS-TIMEOUT: ") && self.stress_expiry.is_none() {
            self.stress_expiry = Some(line.trim().to_string());
        }
    }

    fn expiry_refusal(&self, label: &str) -> Option<String> {
        // Истечение отведённого операции времени не находка, и вердикт
        // называет его отдельно от нарушенного утверждения.
        self.stress_expiry.as_ref().map(|line| {
            format!(
                "QEMU pass '{label}': a stress operation ran out of its allotted time, \
                 not a violated assertion: {line}"
            )
        })
    }

    fn verify(
        &self,
        label: &str,
        expected_seeds: Option<u64>,
        expected_epochs: Option<u64>,
    ) -> Result<()> {
        if let Some(refusal) = self.expiry_refusal(label) {
            bail!(refusal);
        }
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
        if let Some(epochs) = expected_epochs {
            // Эпохи отчитывает один тест набора, поэтому число маркеров на
            // число тестов прохода не умножается.
            let expected = usize::try_from(epochs).unwrap_or(usize::MAX);
            if self.stress_epochs != expected {
                bail!(
                    "QEMU pass '{label}': {} of {expected} stress epochs reported STRESS-EPOCH",
                    self.stress_epochs
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
        assert!(markers.verify("kernel", None, None).is_ok());
    }

    #[test]
    fn a_run_cut_short_is_refused() {
        let mut lines = full_run(3);
        lines.pop();
        assert!(
            observe_all(&borrow(&lines))
                .verify("kernel", None, None)
                .is_err()
        );
    }

    #[test]
    fn a_silently_skipped_test_is_refused() {
        let lines = [
            "[TEST-RUN: 2]",
            "[TEST-START: case_0]",
            "[TEST-PASS: case_0]",
            "[TEST-DONE: 2]",
        ];
        assert!(observe_all(&lines).verify("kernel", None, None).is_err());
    }

    #[test]
    fn a_reported_failure_is_refused() {
        let mut lines = full_run(1);
        lines.insert(1, "[TEST-FAIL: case_0]".to_string());
        assert!(
            observe_all(&borrow(&lines))
                .verify("kernel", None, None)
                .is_err()
        );
    }

    #[test]
    fn a_missing_run_marker_is_refused() {
        assert!(
            observe_all(&["[TEST-PASS: case_0]"])
                .verify("kernel", None, None)
                .is_err()
        );
    }

    #[test]
    fn a_profile_announcing_no_tests_is_refused() {
        let lines = ["[TEST-RUN: 0]", "[TEST-DONE: 0]"];
        assert!(observe_all(&lines).verify("kernel", None, None).is_err());
    }

    fn subrange(seeds: u64, shard: u64, shards: u64) -> SeedShard {
        shard_subrange(1, seeds, shard, shards).expect("the range divides")
    }

    #[test]
    fn shards_cover_the_declared_range_without_overlap() {
        let shards = 4;
        let mut covered = Vec::new();
        for shard in 0..shards {
            let part = subrange(64, shard, shards);
            covered.extend(part.first_seed..part.first_seed + part.seed_count);
        }
        assert_eq!(covered, (1..65).collect::<Vec<_>>());
    }

    #[test]
    fn a_single_shard_takes_the_whole_range() {
        assert_eq!(
            subrange(64, 0, 1),
            SeedShard {
                first_seed: 1,
                seed_count: 64,
            }
        );
    }

    #[test]
    fn a_range_that_wraps_is_split_the_way_the_run_advances_seeds() {
        let starts: Vec<_> = (0..4)
            .map(|shard| {
                shard_subrange(u64::MAX, 4, shard, 4)
                    .expect("the range divides")
                    .first_seed
            })
            .collect();
        assert_eq!(starts, vec![u64::MAX, 0, 1, 2]);
    }

    #[test]
    fn a_range_that_does_not_divide_is_refused() {
        assert!(shard_subrange(1, 65, 0, 4).is_err());
    }

    #[test]
    fn a_shard_outside_the_shard_count_is_refused() {
        assert!(shard_subrange(1, 64, 4, 4).is_err());
        assert!(shard_subrange(1, 64, 0, 0).is_err());
    }

    #[test]
    fn encoded_plan_declares_every_field_on_its_own_line() {
        assert_eq!(
            encode_plan(7, 3, 64, 16),
            "seed=7\nseeds=3\nsteps=64\nepoch=16\n"
        );
    }

    #[test]
    fn a_seed_whose_steps_the_epoch_does_not_divide_gets_one_more() {
        assert_eq!(expected_epochs(64, 16), 4);
        assert_eq!(expected_epochs(70, 32), 3);
        assert_eq!(expected_epochs(1, 32), 1);
    }

    fn stress_run(tests: usize, seeds: usize, epochs: usize) -> Vec<String> {
        let mut lines = full_run(tests);
        lines.push("[STRESS-BASE: budget=512]".to_string());
        for seed in 0..seeds {
            for epoch in 0..epochs {
                lines.push(format!(
                    "[STRESS-EPOCH: seed={seed} epoch={epoch} budget=512]"
                ));
            }
            for _ in 0..tests {
                lines.push(format!("[STRESS-PASS: seed={seed}]"));
            }
        }
        lines
    }

    #[test]
    fn a_complete_stress_report_verifies() {
        let markers = observe_all(&borrow(&stress_run(2, 2, 3)));
        assert_eq!(markers.stress_seeds, 4);
        assert_eq!(markers.stress_epochs, 6);
        assert!(markers.verify("stress", Some(2), Some(6)).is_ok());
    }

    #[test]
    fn every_seed_reports_for_every_stress_test() {
        let markers = observe_all(&borrow(&stress_run(2, 2, 3)));
        assert!(markers.verify("stress", Some(3), Some(6)).is_err());
    }

    #[test]
    fn a_run_short_of_epochs_is_refused() {
        let mut lines = stress_run(2, 2, 3);
        lines.retain(|line| !line.contains("epoch=2"));
        assert!(
            observe_all(&borrow(&lines))
                .verify("stress", Some(2), Some(6))
                .is_err()
        );
    }

    #[test]
    fn a_report_without_epoch_markers_is_refused() {
        let mut lines = stress_run(2, 2, 3);
        lines.retain(|line| !line.contains("[STRESS-EPOCH: "));
        assert!(
            observe_all(&borrow(&lines))
                .verify("stress", Some(2), Some(6))
                .is_err()
        );
    }

    #[test]
    fn an_expired_operation_is_refused_apart_from_a_finding() {
        let mut lines = stress_run(2, 2, 3);
        lines.push(
            "[STRESS-TIMEOUT: seed=1 epoch=2 epoch_seed=9 step=7 operation=port_send_recv \
             outcome=expired phase=recv timeout_ns=1000000000]"
                .to_string(),
        );
        let markers = observe_all(&borrow(&lines));

        let refusal = markers
            .verify("stress", Some(2), Some(6))
            .expect_err("an expiry refuses the run");
        let refusal = refusal.to_string();
        assert!(refusal.contains("ran out of its allotted time"));
        assert!(refusal.contains("operation=port_send_recv"));
    }
}
