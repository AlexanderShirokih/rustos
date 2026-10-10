//! Двухпроходные QEMU-тесты: pass "kernel" и pass "userland".

use std::{
    fs,
    os::unix::process::ExitStatusExt as _,
    path::Path,
    process::{Command, ExitStatus},
    time::{Duration, Instant},
};

use anyhow::{Context, Result, bail};
use userland_image_tool::{BuildEnvironment, ExtraFile};

use crate::{artifacts::build_binary, context::BuildContext, shell, userland};

pub(crate) const DEFAULT_SPEC: &str = "devices/spec/qemu-aarch64-test.yaml";

const TESTRUNNER_IMAGE: &str = "user/testing/testrunner/image.toml";

const STRESS_PLAN_IMAGE_PATH: &str = "/testrunner/stress-plan";

const STRESS_PLAN_SOURCE: &str = "stress-plan.txt";

const TEST_FILTER_IMAGE_PATH: &str = "/testrunner/test-filter";

const TEST_FILTER_SOURCE: &str = "test-filter.txt";

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct Machine {
    pub(crate) cpus: u64,
    pub(crate) memory_mib: u64,
}

#[derive(Debug, Clone, Copy)]
pub(crate) struct StressRun {
    pub(crate) seed: u64,
    pub(crate) seeds: u64,
    pub(crate) steps: u64,
    pub(crate) epoch: u64,
    pub(crate) shard: u64,
    pub(crate) shards: u64,
    pub(crate) machine: Machine,
    pub(crate) timeout: u64,
}

struct QemuPass<'a> {
    label: &'a str,
    spec: &'a Path,
    machine: Option<Machine>,
    features: Option<String>,
    image: &'a Path,
    timeout: u64,
    environment: &'a [BuildEnvironment<'a>],
    files: &'a [ExtraFile],
    filtered: bool,
    expected_seeds: Option<u64>,
    expected_epochs: Option<u64>,
    durations: usize,
}

pub(crate) fn qemu_test(
    timeout: u64,
    spec: &Path,
    filter: Option<&str>,
    durations: usize,
) -> Result<()> {
    let image = Path::new(TESTRUNNER_IMAGE);
    let filter_file = filter_file(filter)?;
    let kernel = qemu_test_pass(QemuPass {
        label: "kernel",
        spec,
        machine: None,
        features: Some(String::from("kernel-tests")),
        image,
        timeout,
        environment: &[],
        files: filter_file.as_slice(),
        filtered: filter.is_some(),
        expected_seeds: None,
        expected_epochs: None,
        durations,
    })?;
    let userland = qemu_test_pass(QemuPass {
        label: "userland",
        spec,
        machine: None,
        features: None,
        image,
        timeout,
        environment: &[],
        files: filter_file.as_slice(),
        filtered: filter.is_some(),
        expected_seeds: None,
        expected_epochs: None,
        durations,
    })?;
    require_match(filter, kernel + userland)
}

pub(crate) fn qemu_bench(timeout: u64, filter: Option<&str>) -> Result<()> {
    let filter_file = filter_file(filter)?;
    let ran = qemu_test_pass(QemuPass {
        label: "bench",
        spec: Path::new(DEFAULT_SPEC),
        machine: None,
        features: None,
        image: Path::new(TESTRUNNER_IMAGE),
        timeout,
        environment: &[BuildEnvironment {
            name: "TEST_PROFILE",
            value: "bench",
        }],
        files: filter_file.as_slice(),
        filtered: filter.is_some(),
        expected_seeds: None,
        expected_epochs: None,
        durations: 0,
    })?;
    require_match(filter, ran)
}

pub(crate) fn qemu_stress(run: StressRun) -> Result<()> {
    let StressRun {
        seed,
        seeds,
        steps,
        epoch,
        shard,
        shards,
        machine,
        timeout,
    } = run;
    if epoch == 0 {
        bail!("--epoch must be greater than zero");
    }
    if machine.cpus == 0 {
        bail!("--cpus must be greater than zero");
    }
    if machine.memory_mib == 0 {
        bail!("--memory must be greater than zero");
    }
    let subrange = shard_subrange(seed, seeds, shard, shards)?;
    let (seed, seeds) = (subrange.first_seed, subrange.seed_count);

    let files = [write_image_file(
        STRESS_PLAN_SOURCE,
        STRESS_PLAN_IMAGE_PATH,
        &encode_plan(seed, seeds, steps, epoch, machine.cpus),
    )?];
    println!(
        "=== stress plan: cargo xtask stress --seed {seed} --seeds {seeds} \
--steps {steps} --epoch {epoch} --cpus {} --memory {} --timeout {timeout} \
(shard {shard} of {shards}) ===",
        machine.cpus, machine.memory_mib
    );

    qemu_test_pass(QemuPass {
        label: "stress",
        spec: Path::new(DEFAULT_SPEC),
        machine: Some(machine),
        // Короткий квант режет окно между операциями и расширяет чередования.
        features: Some(String::from("short-quantum")),
        image: Path::new(TESTRUNNER_IMAGE),
        timeout,
        environment: &[BuildEnvironment {
            name: "TEST_PROFILE",
            value: "stress",
        }],
        files: &files,
        filtered: false,
        expected_seeds: Some(seeds),
        expected_epochs: Some(seeds * expected_epochs(steps, epoch)),
        durations: 0,
    })
    .map(drop)
}

fn write_image_file(source_name: &str, image_path: &str, contents: &str) -> Result<ExtraFile> {
    let project_root = crate::context::project_root();
    let source = crate::context::build_dir(&project_root)?.join(source_name);

    fs::write(&source, contents)?;

    Ok(ExtraFile {
        path: String::from(image_path),
        source,
    })
}

fn filter_file(filter: Option<&str>) -> Result<Option<ExtraFile>> {
    filter
        .map(|filter| write_image_file(TEST_FILTER_SOURCE, TEST_FILTER_IMAGE_PATH, filter))
        .transpose()
}

fn require_match(filter: Option<&str>, ran: usize) -> Result<()> {
    match filter {
        Some(filter) if ran == 0 => bail!("no test name contains '{filter}'"),
        _ => Ok(()),
    }
}

fn encode_plan(seed: u64, seeds: u64, steps: u64, epoch: u64, threads: u64) -> String {
    // Число CPU ABI не сообщает, и ширину параллельной нагрузки прогон получает планом.
    format!("seed={seed}\nseeds={seeds}\nsteps={steps}\nepoch={epoch}\nthreads={threads}\n")
}

fn machine_command(command: &str, machine: Machine) -> Result<String> {
    let mut tokens: Vec<String> = command.split(' ').map(String::from).collect();
    for (option, value) in [
        ("-smp", machine.cpus.to_string()),
        ("-m", format!("{}M", machine.memory_mib)),
    ] {
        let Some(position) = tokens.iter().position(|token| token == option) else {
            bail!("the run command of the spec has no '{option}' to size the machine");
        };
        let Some(slot) = tokens.get_mut(position + 1) else {
            bail!("the run command of the spec has no value after '{option}'");
        };
        *slot = value;
    }
    Ok(tokens.join(" "))
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
    // Деление нацело обязательно, потому что потерянный хвост диапазона оставляет все
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

fn qemu_test_pass(pass: QemuPass<'_>) -> Result<usize> {
    let QemuPass {
        label,
        spec,
        machine,
        features,
        image,
        timeout,
        environment,
        files,
        filtered,
        expected_seeds,
        expected_epochs,
        durations,
    } = pass;
    println!("=== qemu-test pass: {label} ===");

    let ctx = BuildContext::new(spec, features)?;

    userland::build_image(
        &ctx.project_root,
        &ctx.build_dir,
        image,
        &ctx.arch,
        environment,
        files,
    )?;
    build_binary(&ctx)?;

    let qemu_cmd = ctx
        .spec
        .run
        .first()
        .with_context(|| format!("No run commands in {}", spec.display()))?;
    let qemu_cmd = match machine {
        Some(machine) => machine_command(qemu_cmd, machine)?,
        None => qemu_cmd.clone(),
    };

    let mut command = Command::new("sh");
    command
        .args(["-c", &format!("timeout {timeout} {qemu_cmd}")])
        .current_dir(&ctx.project_root);

    let mut markers = TestMarkers::default();
    let mut timings = TestTimings::default();
    let status = shell::run_streaming(&mut command, "QEMU", |line| {
        markers.observe(line);
        timings.observe(line, Instant::now());
    })?;
    let place = match (timings.running(), &markers.stress_started) {
        (Some(test), Some(seed)) => format!(" during {test} ({seed})"),
        (Some(test), None) => format!(" during {test}"),
        (None, _) => String::new(),
    };
    if durations > 0 {
        println!("=== slowest tests of pass {label} ===");
        for (duration, name) in timings.slowest(durations) {
            println!("{:>8.2}s {name}", duration.as_secs_f64());
        }
    }

    if !status.success() {
        if let Some(refusal) = markers.expiry_refusal(label) {
            bail!(refusal);
        }
        if status.code() == Some(124) {
            bail!("QEMU pass '{label}' timed out after {timeout}s{place}");
        }
        bail!("QEMU pass '{label}' {}{place}", exit_description(status));
    }

    markers.verify(label, filtered, expected_seeds, expected_epochs)
}

fn exit_description(status: ExitStatus) -> String {
    match (status.code(), status.signal()) {
        (Some(code), _) => format!("exited with code {code}"),
        (None, Some(signal)) => format!("was killed by signal {signal}"),
        (None, None) => format!("ended with status {status}"),
    }
}

#[derive(Default)]
struct TestTimings {
    running: Option<(String, Instant)>,
    finished: Vec<(Duration, String)>,
}

#[derive(Default)]
struct TestMarkers {
    announced: Option<usize>,
    started: usize,
    passed: usize,
    skipped: usize,
    failed: usize,
    done: Option<usize>,
    stress_seeds: usize,
    stress_epochs: usize,
    stress_expiry: Option<String>,
    stress_started: Option<String>,
}

impl TestTimings {
    fn observe(&mut self, line: &str, now: Instant) {
        if let Some(tail) = line.split("[TEST-START: ").nth(1) {
            let name = tail.split(']').next().unwrap_or(tail);
            self.running = Some((name.to_string(), now));
        } else if ["[TEST-PASS: ", "[TEST-SKIP: ", "[TEST-FAIL"]
            .iter()
            .any(|marker| line.contains(marker))
            && let Some((name, started)) = self.running.take()
        {
            self.finished.push((now - started, name));
        }
    }

    fn running(&self) -> Option<&str> {
        self.running.as_ref().map(|(name, _)| name.as_str())
    }

    fn slowest(mut self, count: usize) -> Vec<(Duration, String)> {
        self.finished
            .sort_by(|(left, _), (right, _)| right.cmp(left));
        self.finished.truncate(count);
        self.finished
    }
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
            self.stress_started = None;
        } else if let Some(tail) = line.split("[STRESS-START: ").nth(1) {
            self.stress_started = tail.split(']').next().map(String::from);
        } else if line.contains("[TEST-PASS: ") {
            self.passed += 1;
        } else if line.contains("[TEST-SKIP: ") {
            self.skipped += 1;
        } else if line.contains("[TEST-FAIL") {
            self.failed += 1;
        } else if line.contains("[TEST-DONE: ") {
            self.done = suffix_count(line, "[TEST-DONE: ");
        } else if line.contains("[STRESS-PASS: ") {
            self.stress_seeds += 1;
            self.stress_started = None;
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
        filtered: bool,
        expected_seeds: Option<u64>,
        expected_epochs: Option<u64>,
    ) -> Result<usize> {
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
        if announced == 0 && !filtered {
            bail!("QEMU pass '{label}': TEST-RUN announced no tests for this profile");
        }
        if self.done != Some(announced) {
            bail!(
                "QEMU pass '{label}': TEST-DONE does not match TEST-RUN ({:?} != {announced}) - \
                 the run was cut short",
                self.done
            );
        }
        if self.started != announced || self.passed + self.skipped != announced {
            bail!(
                "QEMU pass '{label}': of {announced} tests {} started, {} passed and {} \
                 skipped - some tests ended without a verdict",
                self.started,
                self.passed,
                self.skipped
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
        println!(
            "qemu-test pass '{label}': {} tests passed, {} skipped",
            self.passed, self.skipped
        );
        Ok(announced)
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
    fn a_test_lasts_from_its_start_to_its_verdict() {
        let start = Instant::now();
        let mut timings = TestTimings::default();
        let lines = [
            (0, "[I] [testrunner] [TEST-START: quick]"),
            (1, "[I] [testrunner] [TEST-PASS: quick]"),
            (2, "[I] [testrunner] [TEST-START: slow]"),
            (3, "[I] [slow-fixture] output"),
            (
                7,
                "[I] [testrunner] [TEST-SKIP: no device] at src/slow.rs:1",
            ),
            (8, "[I] [testrunner] [TEST-START: medium]"),
            (
                10,
                "[I] [testrunner] [TEST-FAIL: assertion] at src/medium.rs:1",
            ),
        ];
        for (seconds, line) in lines {
            timings.observe(line, start + Duration::from_secs(seconds));
        }

        assert_eq!(
            timings.slowest(2),
            [
                (Duration::from_secs(5), String::from("slow")),
                (Duration::from_secs(2), String::from("medium")),
            ]
        );
    }

    #[test]
    fn a_signal_is_not_reported_as_an_exit_code() {
        assert_eq!(
            exit_description(ExitStatus::from_raw(3 << 8)),
            "exited with code 3"
        );
        assert_eq!(
            exit_description(ExitStatus::from_raw(9)),
            "was killed by signal 9"
        );
    }

    #[test]
    fn a_complete_run_verifies() {
        let lines = full_run(3);
        let markers = observe_all(&borrow(&lines));
        assert_eq!(markers.announced, Some(3));
        assert_eq!(markers.started, 3);
        assert_eq!(markers.passed, 3);
        assert_eq!(markers.verify("kernel", false, None, None).ok(), Some(3));
    }

    #[test]
    fn a_run_cut_short_is_refused() {
        let mut lines = full_run(3);
        lines.pop();
        assert!(
            observe_all(&borrow(&lines))
                .verify("kernel", false, None, None)
                .is_err()
        );
    }

    #[test]
    fn a_test_that_never_started_is_refused() {
        let lines = [
            "[TEST-RUN: 2]",
            "[TEST-START: case_0]",
            "[TEST-PASS: case_0]",
            "[TEST-DONE: 2]",
        ];
        assert!(
            observe_all(&lines)
                .verify("kernel", false, None, None)
                .is_err()
        );
    }

    #[test]
    fn a_skipped_test_completes_the_run() {
        let mut lines = full_run(2);
        lines[4] = "[I] [testrunner] [TEST-SKIP: no device] at simplefb.rs:1".to_string();

        let markers = observe_all(&borrow(&lines));

        assert_eq!(markers.skipped, 1);
        assert!(markers.verify("userland", false, None, None).is_ok());
    }

    #[test]
    fn a_reported_failure_is_refused() {
        let mut lines = full_run(1);
        lines.insert(1, "[TEST-FAIL: case_0]".to_string());
        assert!(
            observe_all(&borrow(&lines))
                .verify("kernel", false, None, None)
                .is_err()
        );
    }

    #[test]
    fn a_missing_run_marker_is_refused() {
        assert!(
            observe_all(&["[TEST-PASS: case_0]"])
                .verify("kernel", false, None, None)
                .is_err()
        );
    }

    #[test]
    fn a_profile_announcing_no_tests_is_refused() {
        let lines = ["[TEST-RUN: 0]", "[TEST-DONE: 0]"];
        assert!(
            observe_all(&lines)
                .verify("kernel", false, None, None)
                .is_err()
        );
    }

    #[test]
    fn a_filtered_pass_without_matching_tests_verifies() {
        let lines = ["[TEST-RUN: 0]", "[TEST-DONE: 0]"];
        assert_eq!(
            observe_all(&lines)
                .verify("kernel", true, None, None)
                .expect("a filter may leave a pass empty"),
            0
        );
    }

    #[test]
    fn a_filter_that_matches_no_test_of_the_command_is_refused() {
        assert!(require_match(Some("missing"), 0).is_err());
        assert!(require_match(Some("liveness"), 2).is_ok());
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
            encode_plan(7, 3, 64, 16, 8),
            "seed=7\nseeds=3\nsteps=64\nepoch=16\nthreads=8\n"
        );
    }

    #[test]
    fn the_machine_replaces_the_size_the_spec_declares() {
        let machine = Machine {
            cpus: 8,
            memory_mib: 4096,
        };
        assert_eq!(
            machine_command("qemu -smp 4 -m 512M -kernel k.bin", machine).expect("sized"),
            "qemu -smp 8 -m 4096M -kernel k.bin"
        );
    }

    #[test]
    fn a_command_without_a_machine_size_is_refused() {
        let machine = Machine {
            cpus: 8,
            memory_mib: 4096,
        };
        assert!(machine_command("qemu -m 512M -kernel k.bin", machine).is_err());
        assert!(machine_command("qemu -smp 4 -m", machine).is_err());
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
        assert!(markers.verify("stress", false, Some(2), Some(6)).is_ok());
    }

    #[test]
    fn every_seed_reports_for_every_stress_test() {
        let markers = observe_all(&borrow(&stress_run(2, 2, 3)));
        assert!(markers.verify("stress", false, Some(3), Some(6)).is_err());
    }

    #[test]
    fn a_run_short_of_epochs_is_refused() {
        let mut lines = stress_run(2, 2, 3);
        lines.retain(|line| !line.contains("epoch=2"));
        assert!(
            observe_all(&borrow(&lines))
                .verify("stress", false, Some(2), Some(6))
                .is_err()
        );
    }

    #[test]
    fn a_report_without_epoch_markers_is_refused() {
        let mut lines = stress_run(2, 2, 3);
        lines.retain(|line| !line.contains("[STRESS-EPOCH: "));
        assert!(
            observe_all(&borrow(&lines))
                .verify("stress", false, Some(2), Some(6))
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
            .verify("stress", false, Some(2), Some(6))
            .expect_err("an expiry refuses the run");
        let refusal = refusal.to_string();
        assert!(refusal.contains("ran out of its allotted time"));
        assert!(refusal.contains("operation=port_send_recv"));
    }
}
