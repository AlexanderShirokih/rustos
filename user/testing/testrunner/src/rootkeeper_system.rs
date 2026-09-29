//! Требования, журнал, отказы, перезапуски, бюджет, выдача устройств `platform` и файловые
//! системы записей композиции rootkeeper.

use alloc::{format, string::String, vec::Vec};

use component_abi::{
    AcceptError, LOG_REQUIREMENT, RequirementDeclaration, SessionError,
    component_startup as startup_contract,
};
use component_startup::{accept, session};
use filesystem_abi::{
    AccessMode, FileSystemError, NodeKind, OpenFlags, file_system, file_system_mount, namespace,
};
use ipc::{Protocol, wire::IpcError};
use kernel_objects::{Error, IrqLine, MemoryRegion, Port, Session, SessionTransport};
use kernel_tests::kernel_test;
use log_abi::log;
use log_relay::SourceNames as _;
use platform_abi::{PlatformError, platform};
use rootkeeper::{
    BootFailure, Stopped,
    relay::{ComponentNames, log_badge},
};
use rtc_abi::rtc;
use runtime::{JoinHandle, PortTransport, Priority, Timeout, spawn_owned};
use syscall::{ProcessName, SyscallError};
use userland::USERLAND_PROGRAM_MAGIC;
use userland_image::{ComponentSpec, PlanDecodeError};

use crate::{
    composition::{
        BOOT_PLAN_ENTRY, Composition, Importer, Mounted, ROUTER_NAME, admin, encoded, image, pause,
        request, request_at, view,
    },
    filesystem::{IMAGE_FILE, connect, open, path},
    log_relay::{
        await_component_lines, await_component_sources, component_scenario_session,
        start_component_relay, stop_component_relay,
    },
    probe_protocol::probe,
};

const MIB: u64 = 1024 * 1024;

const PROBE: &str = "probe";
const HELD: &str = "held";
const STARVED: &str = "starved";
const FILES: &str = "files";
const ROOT_VIEW: &str = "root";
const RTC_REQUIREMENT: &str = "rtc";
const PLATFORM_REQUIREMENT: &str = "platform";
const AREA: &str = "area";
const SPARE_VIEW: &str = "spare";
const TMP_VIEW: &str = "tmp";
const OTHER_VIEW: &str = "other";

const HELD_PATH: &str = "/held";
const WRITTEN_PATH: &str = "/written";
const RENAMED_PATH: &str = "/renamed";
const IMAGE_MOUNT: &str = "/boot";
const NESTED_SPARE_MOUNT: &str = "/boot/spare";
const NESTED_SPARE_PATH: &str = "/boot/spare/x";
const AREA_SCOPE: &str = "/boot/scope-consumer-fixture";

const CALL_WAIT: Timeout = Timeout::from_secs(2);
const REFUSAL_WAIT: Timeout = Timeout::from_secs(1);
const SHORT_WAIT: Timeout = Timeout::from_millis(500);
const QUIET_WAIT: Timeout = Timeout::from_millis(200);
const HELD_WAIT: Timeout = Timeout::from_secs(3);
const BUDGET_WAIT: Timeout = Timeout::from_secs(8);
const REQUEST_WAIT: Timeout = Timeout::from_secs(15);
const RTC_CALL_WAIT: Timeout = Timeout::from_secs(15);
const REVOKE_POLL: Timeout = Timeout::from_millis(10);
const VIEW_WAIT: Timeout = Timeout::from_secs(5);
const OUTSIDE_WAIT: Timeout = Timeout::from_millis(100);

const REVOKE_POLLS: usize = 300;

const REQUEST_STACK_SIZE: u64 = 0x8000;

const TERMINATE_EXIT_CODE: u32 = 7;

const GRANTED: u32 = 0;
const UNAVAILABLE: u32 = 1;
const TIMED_OUT: u32 = 2;
const OTHER: u32 = 3;

const PING: u64 = 0x5A;

const STARVED_MEMORY: u64 = 4096;

const PL031: &str = "arm,pl031";
const VIRTIO: &str = "virtio,mmio";
const ABSENT: &str = "vendor,absent";

const VIRTIO_REGION: u64 = 0xa00_0000;
const VIRTIO_STRIDE: u64 = 0x200;

const PARENT: ComponentSpec<'static> = ComponentSpec {
    executable: "/testrunner",
    name: "testrunner",
    mountpoint: None,
    memory_limit: 0,
    restarts: 0,
};

const EXPORT: ComponentSpec<'static> = fixture("/export-fixture", "export-fixture");
const EXPORT_ENTRY: &str = "/export-fixture/exec";

const IDLE_EXPORT: ComponentSpec<'static> = fixture("/idle-export-fixture", "idle-export-fixture");

const REFUSAL_PROBE: ComponentSpec<'static> = ComponentSpec {
    mountpoint: None,
    ..fixture("/refusal-probe-fixture", "refusal-probe-fixture")
};

const LOG_DUPLICATE: ComponentSpec<'static> = ComponentSpec {
    ..fixture("/log-duplicate-fixture", "log-duplicate-fixture")
};

const NEIGHBOUR: ComponentSpec<'static> = ComponentSpec {
    name: "neighbour",
    ..EXPORT
};

const FAILING: ComponentSpec<'static> = ComponentSpec {
    ..fixture("/failing-fixture", "failing-fixture")
};

const CLOSED: ComponentSpec<'static> = ComponentSpec {
    ..fixture("/managed-startup-closed", "managed-startup-closed")
};

const PROBE_IMPORTER: ComponentSpec<'static> = ComponentSpec {
    mountpoint: None,
    ..fixture("/probe-importer-fixture", "probe-importer-fixture")
};

const HOLDER: ComponentSpec<'static> = ComponentSpec {
    mountpoint: None,
    memory_limit: 2 * MIB,
    restarts: 1,
    ..fixture("/budget-holder-fixture", "budget-holder-fixture")
};

const PLATFORM: ComponentSpec<'static> = ComponentSpec {
    executable: "/platform",
    name: "platform",
    mountpoint: None,
    memory_limit: MIB,
    restarts: 1,
};

const RTC: ComponentSpec<'static> = ComponentSpec {
    executable: "/driver-pl031",
    name: "driver-pl031",
    mountpoint: None,
    memory_limit: MIB,
    restarts: 0,
};

const IRQ: ComponentSpec<'static> = ComponentSpec {
    mountpoint: None,
    restarts: 1,
    ..fixture("/irq-fixture", "irq-fixture")
};

const ROUTER: ComponentSpec<'static> = ComponentSpec {
    executable: "/namespace-router",
    name: ROUTER_NAME,
    mountpoint: None,
    memory_limit: 2 * MIB,
    restarts: 0,
};

const RAM_FS: ComponentSpec<'static> = ComponentSpec {
    executable: "/filesystem-ramfs",
    name: "filesystem-ramfs",
    mountpoint: Some("/tmp"),
    memory_limit: 4 * MIB,
    restarts: 0,
};

const IMAGE_FS: ComponentSpec<'static> = ComponentSpec {
    executable: "/filesystem-imagefs",
    name: "filesystem-imagefs",
    mountpoint: Some(IMAGE_MOUNT),
    ..RAM_FS
};

const SPARE_FS: ComponentSpec<'static> = ComponentSpec {
    executable: "/filesystem-ramfs",
    name: "spare-filesystem",
    mountpoint: Some("/spare"),
    ..RAM_FS
};

const NESTED_SPARE_FS: ComponentSpec<'static> = ComponentSpec {
    mountpoint: Some(NESTED_SPARE_MOUNT),
    ..SPARE_FS
};

const CRASH_FS: ComponentSpec<'static> = ComponentSpec {
    executable: "/crash-filesystem",
    name: "crash-filesystem",
    mountpoint: Some("/crash"),
    restarts: 1,
    ..RAM_FS
};

const SCOPE_CONSUMER: ComponentSpec<'static> = ComponentSpec {
    mountpoint: None,
    ..fixture("/scope-consumer-fixture", "scope-consumer-fixture")
};

pub(crate) const fn fixture(
    executable: &'static str,
    name: &'static str,
) -> ComponentSpec<'static> {
    ComponentSpec {
        executable,
        name,
        mountpoint: None,
        memory_limit: MIB,
        restarts: 0,
    }
}

const fn probe_of(name: &'static str) -> crate::composition::Import {
    crate::composition::Import {
        declaration: RequirementDeclaration::all(name, probe::Protocol::ID),

        namespace_root: None,
    }
}

pub(crate) fn scenario(components: &[ComponentSpec<'_>]) -> Composition {
    started(&[&[PARENT], components].concat())
}

fn started(records: &[ComponentSpec<'_>]) -> Composition {
    let plan = encoded(records);
    start_component_relay(ComponentNames::of(&plan));
    Composition::start(plan, component_scenario_session)
}

pub(crate) fn finish(composition: Composition) {
    drop(composition);
    stop_component_relay();
}

fn ping(importer: &Importer, requirement: &str) -> Result<u64, IpcError> {
    ping_at(importer, requirement, 0)
}

fn ping_at(importer: &Importer, requirement: &str, index: u16) -> Result<u64, IpcError> {
    importer
        .session_at::<probe::Protocol>(requirement, index)
        .client()
        .with_wait(CALL_WAIT)
        .ping(PING)
}

fn requested<P: Protocol>(
    importer: &Importer,
    requirement: &str,
    wait: Timeout,
) -> Result<Result<(), SessionError>, IpcError> {
    request::<P>(importer.handle(), requirement, wait).map(|answer| answer.map(drop))
}

const fn platform_of(name: &'static str) -> crate::composition::Import {
    crate::composition::Import {
        declaration: RequirementDeclaration::all(name, platform::Protocol::ID),
        namespace_root: None,
    }
}

fn platform_session(importer: &Importer, requirement: &str) -> platform::Client<SessionTransport> {
    importer
        .session::<platform::Protocol>(requirement)
        .client()
        .with_wait(CALL_WAIT)
}

fn line(
    session: &platform::Client<SessionTransport>,
    compatible: &str,
) -> Result<IrqLine, PlatformError> {
    session
        .irq(
            ipc::wire::Str::new(compatible).expect("compatible fits"),
            0,
            0,
        )
        .expect("irq call")
        .map(IrqLine::from)
}

fn window(
    session: &platform::Client<SessionTransport>,
    compatible: &str,
    instance: u16,
) -> Result<(MemoryRegion, u64), PlatformError> {
    session
        .mmio(
            ipc::wire::Str::new(compatible).expect("compatible fits"),
            instance,
            0,
        )
        .expect("mmio call")
        .map(|window| (MemoryRegion::from(window.region), window.offset))
}

fn region_base(region: &MemoryRegion) -> u64 {
    region.inspect().expect("region inspects").base
}

pub(crate) fn await_revoked<T>(inspect: impl Fn() -> Result<T, Error>) -> bool {
    (0..REVOKE_POLLS).any(|_| {
        let revoked = inspect().err() == Some(Error::Syscall(SyscallError::Revoked));
        if !revoked {
            pause(REVOKE_POLL);
        }
        revoked
    })
}

fn held_request(importer: &Importer, requirement: &'static str, wait: Timeout) -> JoinHandle {
    let importer = importer.handle();
    spawn_owned(
        move || match request::<probe::Protocol>(importer, requirement, wait) {
            Ok(Ok(_)) => GRANTED,
            Ok(Err(SessionError::Unavailable)) => UNAVAILABLE,
            Err(IpcError::Timeout) => TIMED_OUT,
            _ => OTHER,
        },
        REQUEST_STACK_SIZE,
        Priority::new(1),
    )
    .expect("request thread starts")
}

fn answered(request: &JoinHandle) -> u32 {
    request.wait(REQUEST_WAIT).expect("request thread ends");
    request.exit_code().expect("request thread exit code")
}

fn budget_scenario() -> (Composition, Importer) {
    let composition = scenario(&[ROUTER, RAM_FS, HOLDER, NEIGHBOUR]);
    let importer = composition.importer(&[probe_of(HELD), probe_of(PROBE), view(TMP_VIEW, "/tmp")]);
    kernel_tests::kassert_eq!(await_component_lines(HOLDER.name, 1), ["budget held"]);
    (composition, importer)
}

fn resolve(
    view: &Port,
    text: &str,
    wait: Timeout,
) -> Result<Result<Session<file_system::Protocol>, FileSystemError>, IpcError> {
    namespace::Client::new(PortTransport::new(view))
        .with_wait(wait)
        .resolve(path(text))
        .map(|answer| answer.map(Session::from_session))
}

fn resolved(view: &Port, text: &str) -> file_system::Client<SessionTransport> {
    connect(
        resolve(view, text, VIEW_WAIT)
            .expect("resolve call")
            .expect("path is covered by a mount"),
    )
}

fn write(client: &file_system::Client<SessionTransport>) {
    open(
        client,
        WRITTEN_PATH,
        AccessMode::ReadWrite,
        OpenFlags::CREATE,
    );
}

fn resolution(
    view: &Port,
    text: &str,
    wait: Timeout,
) -> Result<Result<(), FileSystemError>, IpcError> {
    resolve(view, text, wait).map(|answer| answer.map(drop))
}

fn written(client: &file_system::Client<SessionTransport>) -> Result<NodeKind, FileSystemError> {
    client
        .open(path(WRITTEN_PATH), AccessMode::Read, OpenFlags::NONE)
        .expect("open call")
        .map(|opened| opened.kind)
}

fn refusals() -> Vec<String> {
    let composition = scenario(&[EXPORT, REFUSAL_PROBE]);
    let lines = await_component_lines(REFUSAL_PROBE.name, 5);
    finish(composition);
    lines
}

#[kernel_test]
fn a_log_badge_names_its_record() {
    let plan = encoded(&[PARENT, EXPORT, IDLE_EXPORT]);
    let names = ComponentNames::of(&plan);

    for (record, component) in (1..).zip([EXPORT, IDLE_EXPORT]) {
        let name = names
            .name(log_badge(record).get())
            .expect("record badge is named");
        kernel_tests::kassert_eq!(name.as_str(), component.name);
    }
    kernel_tests::kassert!(names.name(log_badge(3).get()).is_none());
}

#[kernel_test]
fn compiled_plan_carries_the_boot_composition() {
    let plan = image()
        .plan(BOOT_PLAN_ENTRY)
        .expect("compiled plan decodes");

    for (name, scope) in [
        ("filesystem-imagefs", IMAGE_MOUNT),
        ("unresponsive-filesystem", "/stuck"),
        ("spare-filesystem", "/spare"),
        ("filesystem-ramfs", "/tmp"),
    ] {
        let component = plan
            .components()
            .find(|component| component.name() == name)
            .expect("plan carries the file system");
        kernel_tests::kassert_eq!(component.mountpoint(), Some(scope));
        let entries = image().entries().expect("image entries");
        let path = format!("{}/exec", component.executable());
        let program = userland::decode_program(entries.entry(&path).expect("program").data())
            .expect("decode program");
        let manifest = component_abi::decode_manifest(userland::ProgramView::manifest(&program))
            .expect("manifest");
        kernel_tests::kassert!(
            manifest
                .exports()
                .any(|protocol| protocol == file_system::Protocol::ID)
        );
        kernel_tests::kassert!(
            manifest
                .requirements()
                .all(|requirement| requirement.protocol() != file_system_mount::Protocol::ID)
        );
    }
}

#[kernel_test]
fn a_program_entry_read_as_a_plan_names_the_boot_failure() {
    let failure = image().plan(EXPORT_ENTRY).err();

    kernel_tests::kassert_eq!(
        failure,
        Some(BootFailure::Plan(PlanDecodeError::InvalidMagic(
            USERLAND_PROGRAM_MAGIC
        )))
    );
    kernel_tests::kassert_eq!(failure.map(BootFailure::stopped), Some(Stopped::BadImage));
}

#[kernel_test]
fn a_requirement_outside_the_record_of_the_caller_is_refused_at_once() {
    kernel_tests::kassert!(
        refusals()
            .iter()
            .any(|line| line == "outside Ok(Err(Refused))")
    );
}

#[kernel_test]
fn a_session_requirement_of_another_protocol_is_refused_at_once() {
    kernel_tests::kassert!(
        refusals()
            .iter()
            .any(|line| line == "protocol Ok(Err(Refused))")
    );
}

#[kernel_test]
fn an_accept_of_an_export_outside_the_record_is_refused_at_once() {
    kernel_tests::kassert!(
        refusals()
            .iter()
            .any(|line| line == "accept Ok(Err(Refused))")
    );
}

#[kernel_test]
fn a_log_requirement_of_another_protocol_is_refused_at_once() {
    // Требования `log` нет в записи плана, и протокол журнала проверяется отдельно от
    // требований записи.
    kernel_tests::kassert!(refusals().iter().any(|line| line == "log Ok(Err(Refused))"));
}

#[kernel_test]
fn a_session_of_another_protocol_exported_by_record_zero_is_refused() {
    let composition = started(&[ComponentSpec {
        executable: "/export-fixture",
        ..PARENT
    }]);
    let importer = composition.importer(&[probe_of(PROBE)]);

    let answer = requested::<probe::Protocol>(&importer, PROBE, CALL_WAIT);

    kernel_tests::kassert_eq!(answer, Ok(Err(SessionError::Refused)));
    finish(composition);
}

#[kernel_test]
fn an_importer_cannot_take_the_requests_of_a_child_export() {
    let composition = scenario(&[EXPORT]);
    let importer = composition.importer(&[probe_of(PROBE)]);
    let parent = startup_contract::Client::new(PortTransport::new(importer.handle()))
        .with_wait(REFUSAL_WAIT);

    let taken = accept::<probe::Protocol>(&parent).map(|answer| answer.map(drop));
    let log = session::<log::Protocol>(&parent, LOG_REQUIREMENT, 0).map(|answer| answer.map(drop));

    kernel_tests::kassert_eq!(taken, Ok(Err(AcceptError::Refused)));
    kernel_tests::kassert_eq!(log, Ok(Err(SessionError::Refused)));
    kernel_tests::kassert_eq!(ping(&importer, PROBE), Ok(PING));
    finish(composition);
}

#[kernel_test]
fn an_importer_stays_waiting_while_its_provider_lives_without_accept() {
    let composition = scenario(&[IDLE_EXPORT]);
    let importer = composition.importer(&[probe_of(PROBE)]);
    kernel_tests::kassert_eq!(await_component_lines(IDLE_EXPORT.name, 1), ["started"]);

    let waited = requested::<probe::Protocol>(&importer, PROBE, SHORT_WAIT);

    kernel_tests::kassert_eq!(waited, Err(IpcError::Timeout));
    finish(composition);
}

#[kernel_test]
fn a_component_logs_under_its_instance_name() {
    let composition = scenario(&[
        ComponentSpec {
            name: "first-probe",
            ..EXPORT
        },
        ComponentSpec {
            name: "second-probe",
            ..EXPORT
        },
    ]);

    let sources = await_component_sources(&["first-probe", "second-probe"]);

    kernel_tests::kassert!(sources.iter().any(|source| source == "first-probe/export"));
    kernel_tests::kassert!(sources.iter().any(|source| source == "second-probe/export"));
    kernel_tests::kassert!(!sources.iter().any(|source| source.starts_with("badge-")));
    finish(composition);
}

#[kernel_test]
fn a_child_log_session_refuses_duplicate() {
    let composition = scenario(&[LOG_DUPLICATE]);

    kernel_tests::kassert_eq!(
        await_component_lines(LOG_DUPLICATE.name, 1),
        ["Err(Syscall(AccessDenied))"]
    );
    finish(composition);
}

#[kernel_test]
fn the_importer_of_a_provider_out_of_restarts_gets_unavailable() {
    let composition = scenario(&[IDLE_EXPORT, NEIGHBOUR]);
    let importer = composition.importer(&[probe_of(HELD), probe_of(PROBE)]);
    kernel_tests::kassert_eq!(await_component_lines(IDLE_EXPORT.name, 1), ["started"]);
    let held = held_request(&importer, HELD, REQUEST_WAIT);
    pause(QUIET_WAIT);

    composition.terminate(IDLE_EXPORT.name, TERMINATE_EXIT_CODE);

    kernel_tests::kassert_eq!(answered(&held), UNAVAILABLE);
    let again = requested::<probe::Protocol>(&importer, HELD, CALL_WAIT);
    kernel_tests::kassert_eq!(again, Ok(Err(SessionError::Unavailable)));
    kernel_tests::kassert_eq!(ping_at(&importer, PROBE, 1), Ok(PING));
    finish(composition);
}

#[kernel_test]
fn a_provider_that_exits_with_code_zero_leaves_its_importers_unavailable() {
    let composition = scenario(&[ComponentSpec {
        restarts: 2,
        ..EXPORT
    }]);
    let importer = composition.importer(&[probe_of(PROBE)]);
    kernel_tests::kassert_eq!(await_component_lines(EXPORT.name, 1), ["started"]);

    composition.terminate(EXPORT.name, 0);

    let answer = requested::<probe::Protocol>(&importer, PROBE, CALL_WAIT);
    kernel_tests::kassert_eq!(answer, Ok(Err(SessionError::Unavailable)));
    kernel_tests::kassert_eq!(await_component_lines(EXPORT.name, 2), ["started"]);
    finish(composition);
}

#[kernel_test]
fn a_failing_child_spends_each_restart_once() {
    let composition = scenario(&[ComponentSpec {
        restarts: 2,
        ..FAILING
    }]);

    kernel_tests::kassert_eq!(await_component_lines(FAILING.name, 3), ["started"; 3]);
    kernel_tests::kassert_eq!(await_component_lines(FAILING.name, 4).len(), 3);
    finish(composition);
}

#[kernel_test]
fn a_child_taken_down_for_a_closed_startup_session_spends_a_restart() {
    let composition = scenario(&[ComponentSpec {
        restarts: 1,
        ..CLOSED
    }]);

    kernel_tests::kassert_eq!(await_component_lines(CLOSED.name, 2).len(), 2);
    kernel_tests::kassert_eq!(await_component_lines(CLOSED.name, 3).len(), 2);
    finish(composition);
}

#[kernel_test]
fn a_child_exit_is_handled_while_an_importer_waits_for_an_accept() {
    let leaving = ComponentSpec {
        name: "leaving",
        restarts: 1,
        ..EXPORT
    };
    let composition = scenario(&[IDLE_EXPORT, leaving]);
    let importer = composition.importer(&[probe_of(HELD)]);
    kernel_tests::kassert_eq!(await_component_lines(IDLE_EXPORT.name, 1), ["started"]);
    kernel_tests::kassert_eq!(await_component_lines(leaving.name, 1), ["started"]);
    let held = held_request(&importer, HELD, HELD_WAIT);
    pause(QUIET_WAIT);

    composition.terminate(leaving.name, TERMINATE_EXIT_CODE);

    kernel_tests::kassert_eq!(
        await_component_lines(leaving.name, 2),
        ["started", "started"]
    );
    kernel_tests::kassert_eq!(answered(&held), TIMED_OUT);
    finish(composition);
}

#[kernel_test]
fn a_provider_restart_leaves_its_importer_running() {
    let composition = scenario(&[
        ComponentSpec {
            restarts: 2,
            ..EXPORT
        },
        PROBE_IMPORTER,
    ]);
    kernel_tests::kassert_eq!(await_component_lines(PROBE_IMPORTER.name, 1), ["started"]);

    for incarnations in 2..=3 {
        composition.terminate(EXPORT.name, TERMINATE_EXIT_CODE);
        kernel_tests::kassert_eq!(
            await_component_lines(EXPORT.name, incarnations).len(),
            incarnations
        );
    }

    kernel_tests::kassert_eq!(await_component_lines(PROBE_IMPORTER.name, 2), ["started"]);
    finish(composition);
}

#[kernel_test]
fn a_component_out_of_restarts_does_not_stop_the_root() {
    let composition = scenario(&[
        ComponentSpec {
            restarts: 1,
            ..ROUTER
        },
        NEIGHBOUR,
    ]);
    let importer = composition.importer(&[view(ROOT_VIEW, ""), probe_of(PROBE)]);
    kernel_tests::kassert_eq!(await_component_lines(ROUTER_NAME, 1), ["started"]);

    composition.terminate(ROUTER_NAME, TERMINATE_EXIT_CODE);
    kernel_tests::kassert_eq!(await_component_lines(ROUTER_NAME, 2).len(), 2);
    composition.terminate(ROUTER_NAME, TERMINATE_EXIT_CODE);

    let refused = requested::<namespace::Protocol>(&importer, ROOT_VIEW, CALL_WAIT);
    kernel_tests::kassert_eq!(refused, Ok(Err(SessionError::Unavailable)));
    kernel_tests::kassert_eq!(ping(&importer, PROBE), Ok(PING));
    finish(composition);
}

#[kernel_test]
fn a_child_is_served_while_another_waits_for_its_budget() {
    let (composition, importer) = budget_scenario();

    let served = request_at::<probe::Protocol>(importer.handle(), PROBE, 1, QUIET_WAIT)
        .map(|answer| answer.map(drop));
    kernel_tests::kassert_eq!(served, Ok(Ok(())));

    let removed = resolved(&importer.port::<namespace::Protocol>(TMP_VIEW), "/")
        .unlink(path(HELD_PATH), NodeKind::File)
        .expect("unlink call");
    kernel_tests::kassert!(removed.is_ok());
    kernel_tests::kassert_eq!(ping(&importer, HELD), Ok(PING));
    finish(composition);
}

#[kernel_test]
fn a_child_whose_budget_did_not_return_in_time_stays_removed() {
    let (composition, importer) = budget_scenario();

    // `BUDGET_WAIT` короче двух `BUDGET_RETURN_TIMEOUT`. Перезапуск вместо снятия добавил бы
    // второе ожидание бюджета, и запрос получил бы `IpcError::Timeout` вместо `Unavailable`.
    let held = requested::<probe::Protocol>(&importer, HELD, BUDGET_WAIT);

    kernel_tests::kassert_eq!(held, Ok(Err(SessionError::Unavailable)));
    kernel_tests::kassert_eq!(ping_at(&importer, PROBE, 1), Ok(PING));
    finish(composition);
}

#[kernel_test]
fn a_launch_short_of_memory_removes_the_child_without_stopping_the_root() {
    let composition = scenario(&[
        ComponentSpec {
            memory_limit: STARVED_MEMORY,
            restarts: 1,
            ..EXPORT
        },
        NEIGHBOUR,
    ]);
    let importer = composition.importer(&[probe_of(STARVED), probe_of(PROBE)]);

    let answer = requested::<probe::Protocol>(&importer, STARVED, CALL_WAIT);

    kernel_tests::kassert_eq!(answer, Ok(Err(SessionError::Unavailable)));
    kernel_tests::kassert_eq!(ping_at(&importer, PROBE, 1), Ok(PING));
    finish(composition);
}

#[kernel_test]
fn a_driver_serves_time_with_the_resources_of_its_platform_session() {
    let composition = scenario(&[PLATFORM, RTC]);
    let importer = composition.importer(&[crate::composition::Import {
        declaration: RequirementDeclaration::all(RTC_REQUIREMENT, rtc::Protocol::ID),

        namespace_root: None,
    }]);
    let clock = importer
        .session::<rtc::Protocol>(RTC_REQUIREMENT)
        .client()
        .with_wait(RTC_CALL_WAIT);

    let now = clock.now().expect("rtc answers the time");
    let ticked = clock.tick().expect("rtc alarm fires");

    kernel_tests::kassert!(ticked >= now);
    drop(clock);
    finish(composition);
}

#[kernel_test]
fn a_restarted_child_takes_the_same_line_and_region_again() {
    let composition = scenario(&[PLATFORM, IRQ]);

    let lines = await_component_lines(IRQ.name, 2);

    kernel_tests::kassert_eq!(lines.len(), 2);
    kernel_tests::kassert!(lines[0].starts_with("line and region "));
    kernel_tests::kassert_eq!(lines[0], lines[1]);
    finish(composition);
}

#[kernel_test]
fn a_compatible_outside_the_platform_description_is_not_described() {
    let composition = scenario(&[PLATFORM]);
    let importer = composition.importer(&[platform_of(PLATFORM_REQUIREMENT)]);

    let session = platform_session(&importer, PLATFORM_REQUIREMENT);

    kernel_tests::kassert_eq!(
        line(&session, ABSENT).map(drop),
        Err(PlatformError::NotDescribed)
    );
    drop(session);
    finish(composition);
}

#[kernel_test]
fn an_instance_outside_the_platform_description_is_not_described() {
    let composition = scenario(&[PLATFORM]);
    let importer = composition.importer(&[platform_of(PLATFORM_REQUIREMENT)]);

    let session = platform_session(&importer, PLATFORM_REQUIREMENT);

    kernel_tests::kassert_eq!(
        window(&session, PL031, 1).map(drop),
        Err(PlatformError::NotDescribed)
    );
    drop(session);
    finish(composition);
}

#[kernel_test]
fn a_line_and_a_region_taken_by_a_session_are_granted_to_another_after_it_closes() {
    let composition = scenario(&[PLATFORM]);
    let importer = composition.importer(&[platform_of(PLATFORM_REQUIREMENT)]);
    let first = platform_session(&importer, PLATFORM_REQUIREMENT);
    let second = platform_session(&importer, PLATFORM_REQUIREMENT);
    let held_line = line(&first, PL031).expect("first session takes the line");
    let (held_region, _) = window(&first, PL031, 0).expect("first session takes the region");

    kernel_tests::kassert_eq!(line(&second, PL031).map(drop), Err(PlatformError::Taken));
    kernel_tests::kassert_eq!(
        window(&second, PL031, 0).map(drop),
        Err(PlatformError::Taken)
    );

    drop(first);

    kernel_tests::kassert!(await_revoked(|| held_region.inspect()));
    kernel_tests::kassert!(await_revoked(|| held_line.ack()));
    kernel_tests::kassert!(line(&second, PL031).is_ok());
    kernel_tests::kassert!(window(&second, PL031, 0).is_ok());
    drop(second);
    finish(composition);
}

#[kernel_test]
fn a_window_in_the_region_of_another_session_is_taken() {
    let composition = scenario(&[PLATFORM]);
    let importer = composition.importer(&[platform_of(PLATFORM_REQUIREMENT)]);
    let first = platform_session(&importer, PLATFORM_REQUIREMENT);
    let second = platform_session(&importer, PLATFORM_REQUIREMENT);

    let taken = window(&first, VIRTIO, 0);

    kernel_tests::kassert!(taken.is_ok());
    kernel_tests::kassert_eq!(
        window(&second, VIRTIO, 1).map(drop),
        Err(PlatformError::Taken)
    );
    drop((first, second, taken));
    finish(composition);
}

#[kernel_test]
fn windows_of_one_session_in_one_page_share_the_region_at_their_offsets() {
    let composition = scenario(&[PLATFORM]);
    let importer = composition.importer(&[platform_of(PLATFORM_REQUIREMENT)]);
    let session = platform_session(&importer, PLATFORM_REQUIREMENT);

    let (first, first_offset) = window(&session, VIRTIO, 0).expect("first window");
    let (second, second_offset) = window(&session, VIRTIO, 1).expect("second window");

    kernel_tests::kassert_eq!(region_base(&first), VIRTIO_REGION);
    kernel_tests::kassert_eq!(region_base(&second), VIRTIO_REGION);
    kernel_tests::kassert_eq!((first_offset, second_offset), (0, VIRTIO_STRIDE));
    drop(session);
    finish(composition);
}

#[kernel_test]
fn the_grants_of_a_platform_incarnation_are_revoked_when_it_ends() {
    let composition = scenario(&[PLATFORM]);
    let importer = composition.importer(&[platform_of(PLATFORM_REQUIREMENT)]);
    let session = platform_session(&importer, PLATFORM_REQUIREMENT);
    let held_line = line(&session, PL031).expect("session takes the line");
    let (held_region, _) = window(&session, PL031, 0).expect("session takes the region");

    composition.terminate(PLATFORM.name, TERMINATE_EXIT_CODE);

    kernel_tests::kassert!(await_revoked(|| held_region.inspect()));
    kernel_tests::kassert!(await_revoked(|| held_line.ack()));
    let renewed = platform_session(&importer, PLATFORM_REQUIREMENT);
    kernel_tests::kassert!(line(&renewed, PL031).is_ok());
    kernel_tests::kassert!(window(&renewed, PL031, 0).is_ok());
    drop((session, renewed));
    finish(composition);
}

#[kernel_test]
fn the_image_region_of_an_image_file_system_incarnation_is_revoked_when_it_ends() {
    let composition = scenario(&[ROUTER, IMAGE_FS]);
    let importer = composition.importer(&[view(ROOT_VIEW, "")]);
    let files = resolved(
        &importer.port::<namespace::Protocol>(ROOT_VIEW),
        IMAGE_MOUNT,
    );
    let opened = open(&files, IMAGE_FILE, AccessMode::Read, OpenFlags::NONE);
    let window = MemoryRegion::from(
        files
            .backing_memory(opened.fd)
            .expect("backing_memory call")
            .expect("window granted"),
    );
    kernel_tests::kassert!(window.inspect().is_ok());

    composition.terminate(IMAGE_FS.name, TERMINATE_EXIT_CODE);

    kernel_tests::kassert!(await_revoked(|| window.inspect()));
    drop(files);
    finish(composition);
}

#[kernel_test]
fn a_directory_session_is_opened_anew_for_every_incarnation() {
    let composition = scenario(&[
        ROUTER,
        RAM_FS,
        IMAGE_FS,
        ComponentSpec {
            restarts: 1,
            ..SCOPE_CONSUMER
        },
    ]);

    let importer = composition.importer(&[
        view(ROOT_VIEW, ""),
        view(AREA, AREA_SCOPE),
        admin("area-admin"),
    ]);
    let root = importer.port::<namespace::Protocol>(ROOT_VIEW);
    let directory = resolve(&root, AREA_SCOPE, VIEW_WAIT)
        .expect("resolve call")
        .expect("component directory exists");
    let mounted = Mounted::new(&importer, AREA, directory);

    kernel_tests::kassert_eq!(
        await_component_lines(SCOPE_CONSUMER.name, 2),
        ["area read", "area read"]
    );
    drop(mounted);
    finish(composition);
}

#[kernel_test]
fn a_component_that_faults_is_restarted() {
    let composition = scenario(&[ROUTER, CRASH_FS]);
    let importer = composition.importer(&[view(FILES, "/crash")]);
    let filesystem = importer.port::<namespace::Protocol>(FILES);
    let faulting = resolved(&filesystem, "/");
    write(&faulting);

    let renamed = faulting.rename(path(WRITTEN_PATH), path(RENAMED_PATH));

    kernel_tests::kassert!(renamed.is_err());
    let restarted = resolved(&filesystem, "/");
    kernel_tests::kassert_eq!(written(&restarted), Err(FileSystemError::NotFound));
    finish(composition);
}

#[kernel_test]
fn a_restarted_file_system_serves_its_prefix_again_without_restarting_the_router() {
    let composition = scenario(&[
        ROUTER,
        ComponentSpec {
            restarts: 1,
            ..RAM_FS
        },
    ]);
    let importer = composition.importer(&[view(TMP_VIEW, "/tmp")]);
    let tmp = importer.port::<namespace::Protocol>(TMP_VIEW);
    write(&resolved(&tmp, "/"));

    composition.terminate(RAM_FS.name, TERMINATE_EXIT_CODE);

    kernel_tests::kassert_eq!(
        await_component_lines(RAM_FS.name, 2),
        ["started", "started"]
    );
    kernel_tests::kassert_eq!(
        written(&resolved(&tmp, "/")),
        Err(FileSystemError::NotFound)
    );
    finish(composition);
}

#[kernel_test]
fn a_restarted_router_is_mounted_again_by_every_file_system() {
    let composition = scenario(&[
        ComponentSpec {
            restarts: 1,
            ..ROUTER
        },
        RAM_FS,
        IMAGE_FS,
    ]);
    let importer = composition.importer(&[view(ROOT_VIEW, ""), view(TMP_VIEW, "/tmp")]);
    let before = importer.port::<namespace::Protocol>(ROOT_VIEW);
    kernel_tests::kassert_eq!(resolution(&before, IMAGE_MOUNT, VIEW_WAIT), Ok(Ok(())));

    composition.terminate(ROUTER_NAME, TERMINATE_EXIT_CODE);

    for (requirement, text) in [(ROOT_VIEW, IMAGE_MOUNT), (TMP_VIEW, "/")] {
        let renewed = importer.port::<namespace::Protocol>(requirement);
        kernel_tests::kassert_eq!(resolution(&renewed, text, VIEW_WAIT), Ok(Ok(())));
    }
    kernel_tests::kassert!(resolution(&before, IMAGE_MOUNT, SHORT_WAIT).is_err());
    finish(composition);
}

#[kernel_test]
fn router_restart_keeps_file_system_components_and_their_sessions() {
    let composition = scenario(&[
        ComponentSpec {
            restarts: 1,
            ..ROUTER
        },
        RAM_FS,
    ]);
    let importer = composition.importer(&[view(TMP_VIEW, "/tmp")]);
    let files = resolved(&importer.port::<namespace::Protocol>(TMP_VIEW), "/");
    write(&files);

    composition.terminate(ROUTER_NAME, TERMINATE_EXIT_CODE);

    let tmp = resolved(&importer.port::<namespace::Protocol>(TMP_VIEW), "/");
    kernel_tests::kassert_eq!(written(&tmp), Ok(NodeKind::File));
    kernel_tests::kassert_eq!(written(&files), Ok(NodeKind::File));
    finish(composition);
}

#[kernel_test]
fn a_removed_file_system_frees_its_prefix() {
    let composition = scenario(&[ROUTER, IMAGE_FS, NESTED_SPARE_FS]);
    let importer =
        composition.importer(&[view(ROOT_VIEW, ""), view(SPARE_VIEW, NESTED_SPARE_MOUNT)]);
    let spare = importer.port::<namespace::Protocol>(SPARE_VIEW);
    kernel_tests::kassert_eq!(resolution(&spare, "/", VIEW_WAIT), Ok(Ok(())));

    composition.terminate(NESTED_SPARE_FS.name, TERMINATE_EXIT_CODE);

    kernel_tests::kassert_eq!(
        resolution(&spare, "/", OUTSIDE_WAIT),
        Err(IpcError::Timeout)
    );
    let root = importer.port::<namespace::Protocol>(ROOT_VIEW);
    kernel_tests::kassert_eq!(
        resolution(&root, NESTED_SPARE_PATH, VIEW_WAIT),
        Ok(Err(FileSystemError::NotFound))
    );
    finish(composition);
}

#[kernel_test]
fn a_mount_ends_when_its_file_system_closes_its_port() {
    let composition = scenario(&[ROUTER, SPARE_FS]);
    let importer = composition.importer(&[
        view(OTHER_VIEW, "/other"),
        admin("other-admin"),
        view(SPARE_VIEW, "/spare"),
    ]);
    let mounted = Mounted::new(
        &importer,
        OTHER_VIEW,
        Session::from_session(
            resolve(
                &importer.port::<namespace::Protocol>(SPARE_VIEW),
                "/",
                VIEW_WAIT,
            )
            .expect("resolve call")
            .expect("spare mounted"),
        ),
    );
    let other = importer.port::<namespace::Protocol>(OTHER_VIEW);

    composition.terminate(SPARE_FS.name, TERMINATE_EXIT_CODE);

    kernel_tests::kassert_eq!(
        resolution(&other, "/", OUTSIDE_WAIT),
        Err(IpcError::Timeout)
    );
    kernel_tests::kassert!(mounted.ended(CALL_WAIT));
    drop(mounted);
    finish(composition);
}

#[kernel_test]
fn optional_and_all_start_with_empty_or_populated_provider_lists() {
    for providers in [&[][..], &[1][..]] {
        let consumer = ComponentSpec {
            ..fixture("/multiplicity-fixture", "multiplicity")
        };
        let composition = if providers.is_empty() {
            scenario(&[consumer])
        } else {
            scenario(&[EXPORT, consumer])
        };
        let provider_count = if providers.is_empty() {
            "providers 0"
        } else {
            "providers 1"
        };

        kernel_tests::kassert_eq!(
            await_component_lines(consumer.name, 4),
            ["optional empty", "started", provider_count, "unbound"]
        );
        finish(composition);
    }
}

#[kernel_test]
fn a_started_consumer_discovers_a_provider_added_at_runtime() {
    let consumer = ComponentSpec {
        ..fixture("/multiplicity-fixture", "multiplicity")
    };
    let plan = encoded(&[PARENT, consumer]);
    let names = ComponentNames::of(&plan);
    start_component_relay(names.clone());
    let composition = Composition::start(plan, component_scenario_session);
    kernel_tests::kassert_eq!(
        await_component_lines(consumer.name, 4),
        ["optional empty", "started", "providers 0", "unbound"]
    );

    let entry = image()
        .entries()
        .expect("image entries")
        .entry(EXPORT_ENTRY)
        .expect("export program");
    let program = userland::decode_program(entry.data()).expect("decoded program");
    let badge = names.add(EXPORT.name).expect("log source added");
    let log = component_scenario_session(badge).expect("log session");
    composition.add(
        rootkeeper::Component {
            name: ProcessName::new(EXPORT.name).expect("fixture process name"),
            program,
            memory_limit: EXPORT.memory_limit,
            restarts: 0,
        },
        log,
    );

    kernel_tests::kassert_eq!(
        await_component_lines(consumer.name, 5),
        [
            "optional empty",
            "started",
            "providers 0",
            "unbound",
            "late provider"
        ]
    );
    finish(composition);
}

#[kernel_test]
fn registration_refuses_invalid_declarations_without_stopping_the_parent() {
    let plan = encoded(&[PARENT, EXPORT]);
    let names = ComponentNames::of(&plan);
    start_component_relay(names.clone());
    let composition = Composition::start(plan, component_scenario_session);
    let importer = composition.importer(&[probe_of(PROBE)]);
    let entry = image()
        .entries()
        .expect("entries")
        .entry(EXPORT_ENTRY)
        .expect("program");
    let program = userland::decode_program(entry.data()).expect("program decodes");
    let badge = names.add("registered").expect("log source");
    let component = |program| rootkeeper::Component {
        name: ProcessName::new("registered").expect("process name"),
        program,
        memory_limit: EXPORT.memory_limit,
        restarts: 0,
    };

    let mut bytes = entry.data().to_vec();
    let manifest_at = 31 + usize::from(u16::from_le_bytes([bytes[25], bytes[26]])) * 40;
    bytes[manifest_at] ^= 1;
    let damaged =
        userland::decode_program(Vec::leak(bytes)).expect("program boundaries remain valid");
    let failure = composition.try_add(
        component(damaged),
        component_scenario_session(badge).expect("log"),
    );
    kernel_tests::kassert!(matches!(failure, Err(rootkeeper::AddFailure::Declaration)));
    kernel_tests::kassert_eq!(ping(&importer, PROBE), Ok(PING));

    let record = composition
        .try_add(
            component(program),
            component_scenario_session(badge).expect("log"),
        )
        .expect("same name can register after refusal");
    kernel_tests::kassert_eq!(record, 2);
    kernel_tests::kassert_eq!(ping_at(&importer, PROBE, 1), Ok(PING));
    finish(composition);
}

#[kernel_test]
fn two_instances_of_one_program_keep_their_mounts_separate() {
    let composition = scenario(&[ROUTER, RAM_FS, SPARE_FS]);
    let importer = composition.importer(&[view(TMP_VIEW, "/tmp"), view(SPARE_VIEW, "/spare")]);
    let first = resolved(&importer.port::<namespace::Protocol>(TMP_VIEW), "/");
    let second = resolved(&importer.port::<namespace::Protocol>(SPARE_VIEW), "/");

    write(&first);

    kernel_tests::kassert_eq!(written(&first), Ok(NodeKind::File));
    kernel_tests::kassert_eq!(written(&second), Err(FileSystemError::NotFound));
    finish(composition);
}
