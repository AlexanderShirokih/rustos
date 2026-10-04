//! Требования, журнал, отказы, перезапуски, бюджет, выдача устройств `platform` и файловые
//! системы записей композиции rootkeeper.

use alloc::{format, string::String, vec::Vec};

use bootstrap_abi::hardware;
use component_abi::{
    AcceptError, Attribute, LOG_REQUIREMENT, RequirementDeclaration, STARTUP_REQUEST_TIMEOUT,
    SessionError, SessionRequirement, attributes_len, component_startup as startup_contract,
    encode_attributes_into,
};
use component_startup::{Attributes, Found, accept, session};
use filesystem_abi::{
    AccessMode, FileSystemError, NodeKind, OpenFlags, file_system, file_system_mount, namespace,
};
use ipc::{
    Protocol,
    wire::{Bytes, IpcError, Str},
};
use kernel_objects::{Error, IrqLine, MemoryRegion, Port, Session, SessionTransport};
use kernel_tests::kernel_test;
use log_abi::{LogBuffer, log};
use log_relay::SourceNames as _;
use platform_abi::{Description, MmioWindow, PlatformError, platform};
use rootkeeper::{
    BootFailure, Stopped,
    relay::{ComponentNames, log_badge},
};
use rtc_abi::rtc;
use runtime::{JoinHandle, PortTransport, Priority, Timeout, spawn_owned};
use syscall::{ProcessName, Rights, SyscallError};
use userland::USERLAND_PROGRAM_MAGIC;
use userland_image::{ComponentSpec, PlanDecodeError};

use crate::{
    composition::{
        BOOT_PLAN_ENTRY, Composition, Importer, Mounted, ROUTER_NAME, admin, drained, encoded,
        image, pause, request, view,
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
const HARDWARE_REQUIREMENT: &str = "hardware";

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
const SEARCH_CYCLES: usize = 300;

const REQUEST_STACK_SIZE: u64 = 0x8000;

const TERMINATE_EXIT_CODE: u32 = 7;

const GRANTED: u32 = 0;
const UNAVAILABLE: u32 = 1;
const TIMED_OUT: u32 = 2;
const OTHER: u32 = 3;

const PING: u64 = 0x5A;

const STARVED_MEMORY: u64 = 4096;

const PL031: &str = "/pl031@9010000";
const FIRST_VIRTIO: &str = "/virtio_mmio@a000000";
const SECOND_VIRTIO: &str = "/virtio_mmio@a000200";
const ABSENT: &str = "/absent@0";
const RELATIVE: &str = "pl031@9010000";

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

const FILTERED: ComponentSpec<'static> =
    fixture("/filtered-export-fixture", "filtered-export-fixture");

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
    importer
        .session::<probe::Protocol>(requirement)
        .client()
        .with_wait(CALL_WAIT)
        .ping(PING)
}

fn ping_found(found: &Found) -> Result<u64, IpcError> {
    component_startup::open::<probe::Protocol>(found, CALL_WAIT)?
        .map_err(|_| IpcError::PeerClosed)?
        .client()
        .with_wait(CALL_WAIT)
        .ping(PING)
}

fn first_found<P: Protocol>(importer: &Importer, requirement: &str) -> Found {
    importer
        .found::<P>(requirement)
        .into_iter()
        .next()
        .expect("a provider is found")
}

fn await_found(importer: &Importer, requirement: &str, count: usize) -> Vec<Found> {
    let mut found = importer.found::<probe::Protocol>(requirement);
    for _ in 0..REVOKE_POLLS {
        if found.len() == count {
            break;
        }
        pause(REVOKE_POLL);
        found = importer.found::<probe::Protocol>(requirement);
    }
    found
}

fn enumerated(importer: &Importer, requirement: &str) -> Session<startup_contract::Protocol> {
    importer
        .enumeration::<probe::Protocol>(requirement, Attributes::empty())
        .expect("all call")
        .expect("enumeration granted")
}

const fn pair(key: &'static str, value: &'static str) -> Attribute<'static> {
    Attribute {
        key: key.as_bytes(),
        value: value.as_bytes(),
    }
}

fn query(pairs: &[Attribute<'_>]) -> Vec<u8> {
    let mut bytes = alloc::vec![0; attributes_len(pairs)];
    encode_attributes_into(&mut bytes, pairs);
    bytes
}

fn text(attributes: Attributes<'_>) -> String {
    attributes
        .iter()
        .map(|pair| {
            format!(
                "{}={}",
                core::str::from_utf8(pair.key).expect("utf-8 key"),
                core::str::from_utf8(pair.value).expect("utf-8 value")
            )
        })
        .collect::<Vec<_>>()
        .join(",")
}

fn filters_found(importer: &Importer, pairs: &[Attribute<'_>]) -> Vec<String> {
    let bytes = query(pairs);
    importer
        .found_with::<probe::Protocol>(PROBE, Attributes::decode(&bytes).expect("query decodes"))
        .iter()
        .map(|found| text(found.filter.as_attributes()))
        .collect()
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
    device: &str,
) -> Result<IrqLine, PlatformError> {
    session
        .irq(Str::new(device).expect("device fits"), 0)
        .expect("irq call")
        .map(IrqLine::from)
}

fn window(
    session: &platform::Client<SessionTransport>,
    device: &str,
) -> Result<MmioWindow<MemoryRegion>, PlatformError> {
    session
        .mmio(Str::new(device).expect("device fits"), 0)
        .expect("mmio call")
        .map(|window| MmioWindow {
            region: MemoryRegion::from(window.region),
            offset: window.offset,
            size: window.size,
        })
}

fn description(
    session: &platform::Client<SessionTransport>,
) -> Result<(MemoryRegion, u64), PlatformError> {
    session
        .description()
        .expect("description call")
        .map(|description| (MemoryRegion::from(description.region), description.offset))
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

fn held_request(found: Found, wait: Timeout) -> JoinHandle {
    spawn_owned(
        move || match component_startup::open::<probe::Protocol>(&found, wait) {
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

    let owners = [1, 2].map(|record| {
        names
            .name(log_badge(record).get())
            .expect("record badge is named")
    });
    let unnamed = names.name(log_badge(3).get());

    kernel_tests::kassert_eq!(
        owners.each_ref().map(LogBuffer::as_str),
        [EXPORT.name, IDLE_EXPORT.name]
    );
    kernel_tests::kassert!(unnamed.is_none());
}

#[kernel_test]
fn compiled_plan_carries_the_boot_composition() {
    let plan = image()
        .plan(BOOT_PLAN_ENTRY)
        .expect("compiled plan decodes");

    let file_systems = [
        ("filesystem-imagefs", IMAGE_MOUNT),
        ("unresponsive-filesystem", "/stuck"),
        ("spare-filesystem", "/spare"),
        ("filesystem-ramfs", "/tmp"),
    ]
    .map(|(name, scope)| {
        let component = plan
            .components()
            .find(|component| component.name() == name)
            .expect("plan carries the file system");
        let entries = image().entries().expect("image entries");
        let path = format!("{}/exec", component.executable());
        let program = userland::decode_program(entries.entry(&path).expect("program").data())
            .expect("decode program");
        let manifest = component_abi::decode_manifest(userland::ProgramView::manifest(&program))
            .expect("manifest");
        let serves = manifest
            .exports()
            .any(|export| export.protocol() == file_system::Protocol::ID);
        let mounts_nothing = manifest
            .requirements()
            .all(|requirement| requirement.protocol() != file_system_mount::Protocol::ID);
        (scope, component.mountpoint(), serves, mounts_nothing)
    });

    for (scope, mountpoint, serves, mounts_nothing) in file_systems {
        kernel_tests::kassert_eq!(mountpoint, Some(scope));
        kernel_tests::kassert!(serves);
        kernel_tests::kassert!(mounts_nothing);
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
    let lines = refusals();

    kernel_tests::kassert!(lines.iter().any(|line| line == "outside Ok(Err(Refused))"));
}

#[kernel_test]
fn a_session_requirement_of_another_protocol_is_refused_at_once() {
    let lines = refusals();

    kernel_tests::kassert!(lines.iter().any(|line| line == "protocol Ok(Err(Refused))"));
}

#[kernel_test]
fn an_accept_of_an_export_outside_the_record_is_refused_at_once() {
    let lines = refusals();

    kernel_tests::kassert!(lines.iter().any(|line| line == "accept Ok(Err(Refused))"));
}

#[kernel_test]
fn a_log_requirement_of_another_protocol_is_refused_at_once() {
    let lines = refusals();

    kernel_tests::kassert!(lines.iter().any(|line| line == "log Ok(Err(Refused))"));
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
    let log = session::<log::Protocol>(&parent, LOG_REQUIREMENT, Attributes::empty())
        .map(|answer| answer.map(drop));
    let pinged = ping(&importer, PROBE);

    kernel_tests::kassert_eq!(taken, Ok(Err(AcceptError::Refused)));
    kernel_tests::kassert_eq!(log, Ok(Err(SessionError::Refused)));
    kernel_tests::kassert_eq!(pinged, Ok(PING));
    finish(composition);
}

#[kernel_test]
fn an_importer_stays_waiting_while_its_provider_lives_without_accept() {
    let composition = scenario(&[IDLE_EXPORT]);
    let importer = composition.importer(&[probe_of(PROBE)]);
    let started = await_component_lines(IDLE_EXPORT.name, 1);

    let waited = requested::<probe::Protocol>(&importer, PROBE, SHORT_WAIT);

    kernel_tests::kassert_eq!(started, ["started"]);
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

    let lines = await_component_lines(LOG_DUPLICATE.name, 1);

    kernel_tests::kassert_eq!(lines, ["Err(Syscall(AccessDenied))"]);
    finish(composition);
}

#[kernel_test]
fn the_importer_of_a_provider_out_of_restarts_gets_unavailable() {
    let composition = scenario(&[IDLE_EXPORT, NEIGHBOUR]);
    let importer = composition.importer(&[probe_of(HELD), probe_of(PROBE)]);
    let started = await_component_lines(IDLE_EXPORT.name, 1);
    let held = held_request(
        first_found::<probe::Protocol>(&importer, HELD),
        REQUEST_WAIT,
    );
    let again = first_found::<probe::Protocol>(&importer, HELD);
    pause(QUIET_WAIT);

    composition.terminate(IDLE_EXPORT.name, TERMINATE_EXIT_CODE);
    let held_answer = answered(&held);
    let again = component_startup::open::<probe::Protocol>(&again, CALL_WAIT)
        .map(|answer| answer.map(drop));
    let pinged = ping(&importer, PROBE);

    kernel_tests::kassert_eq!(started, ["started"]);
    kernel_tests::kassert_eq!(held_answer, UNAVAILABLE);
    kernel_tests::kassert_eq!(again, Ok(Err(SessionError::Unavailable)));
    kernel_tests::kassert_eq!(pinged, Ok(PING));
    finish(composition);
}

#[kernel_test]
fn a_provider_that_exits_with_code_zero_leaves_its_importers_unavailable() {
    let composition = scenario(&[ComponentSpec {
        restarts: 2,
        ..EXPORT
    }]);
    let importer = composition.importer(&[probe_of(PROBE)]);
    let started = await_component_lines(EXPORT.name, 1);
    let found = first_found::<probe::Protocol>(&importer, PROBE);

    composition.terminate(EXPORT.name, 0);
    let answer = component_startup::open::<probe::Protocol>(&found, CALL_WAIT)
        .map(|answer| answer.map(drop));
    let again = requested::<probe::Protocol>(&importer, PROBE, CALL_WAIT);
    let lines = await_component_lines(EXPORT.name, 2);

    kernel_tests::kassert_eq!(started, ["started"]);
    kernel_tests::kassert_eq!(answer, Ok(Err(SessionError::Unavailable)));
    kernel_tests::kassert_eq!(again, Ok(Err(SessionError::Unbound)));
    kernel_tests::kassert_eq!(lines, ["started"]);
    finish(composition);
}

#[kernel_test]
fn a_failing_child_spends_each_restart_once() {
    let composition = scenario(&[ComponentSpec {
        restarts: 2,
        ..LOG_DUPLICATE
    }]);

    let failures = await_component_lines(LOG_DUPLICATE.name, 3);
    let after = await_component_lines(LOG_DUPLICATE.name, 4);

    kernel_tests::kassert_eq!(failures, ["Err(Syscall(AccessDenied))"; 3]);
    kernel_tests::kassert_eq!(after.len(), 3);
    finish(composition);
}

#[kernel_test]
fn a_child_taken_down_for_a_closed_startup_session_spends_a_restart() {
    let composition = scenario(&[ComponentSpec {
        restarts: 1,
        ..CLOSED
    }]);

    let restarted = await_component_lines(CLOSED.name, 2);
    let after = await_component_lines(CLOSED.name, 3);

    kernel_tests::kassert_eq!(restarted.len(), 2);
    kernel_tests::kassert_eq!(after.len(), 2);
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
    let idle_started = await_component_lines(IDLE_EXPORT.name, 1);
    let leaving_started = await_component_lines(leaving.name, 1);
    let held = held_request(first_found::<probe::Protocol>(&importer, HELD), HELD_WAIT);
    pause(QUIET_WAIT);

    composition.terminate(leaving.name, TERMINATE_EXIT_CODE);
    let restarted = await_component_lines(leaving.name, 2);
    let held_answer = answered(&held);

    kernel_tests::kassert_eq!(idle_started, ["started"]);
    kernel_tests::kassert_eq!(leaving_started, ["started"]);
    kernel_tests::kassert_eq!(restarted, ["started", "started"]);
    kernel_tests::kassert_eq!(held_answer, TIMED_OUT);
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
    let started = await_component_lines(PROBE_IMPORTER.name, 1);

    let incarnations = [2, 3].map(|incarnations| {
        composition.terminate(EXPORT.name, TERMINATE_EXIT_CODE);
        await_component_lines(EXPORT.name, incarnations).len()
    });
    let lines = await_component_lines(PROBE_IMPORTER.name, 2);

    kernel_tests::kassert_eq!(started, ["started"]);
    kernel_tests::kassert_eq!(incarnations, [2, 3]);
    kernel_tests::kassert_eq!(lines, ["started"]);
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
    let started = await_component_lines(ROUTER_NAME, 1);
    let router = first_found::<namespace::Protocol>(&importer, ROOT_VIEW);

    composition.terminate(ROUTER_NAME, TERMINATE_EXIT_CODE);
    let restarted = await_component_lines(ROUTER_NAME, 2);
    composition.terminate(ROUTER_NAME, TERMINATE_EXIT_CODE);
    let refused = component_startup::open::<namespace::Protocol>(&router, CALL_WAIT)
        .map(|answer| answer.map(drop));
    let pinged = ping(&importer, PROBE);

    kernel_tests::kassert_eq!(started, ["started"]);
    kernel_tests::kassert_eq!(restarted.len(), 2);
    kernel_tests::kassert_eq!(refused, Ok(Err(SessionError::Unavailable)));
    kernel_tests::kassert_eq!(pinged, Ok(PING));
    finish(composition);
}

#[kernel_test]
fn a_child_is_served_while_another_waits_for_its_budget() {
    let (composition, importer) = budget_scenario();

    let found = importer.found::<probe::Protocol>(PROBE);
    let served = found.get(1).map(|found| {
        component_startup::open::<probe::Protocol>(found, QUIET_WAIT).map(|answer| answer.map(drop))
    });
    let removed = resolved(&importer.port::<namespace::Protocol>(TMP_VIEW), "/")
        .unlink(path(HELD_PATH), NodeKind::File)
        .expect("unlink call");
    let pinged = ping_found(&first_found::<probe::Protocol>(&importer, HELD));

    kernel_tests::kassert_eq!(served, Some(Ok(Ok(()))));
    kernel_tests::kassert!(removed.is_ok());
    kernel_tests::kassert_eq!(pinged, Ok(PING));
    finish(composition);
}

#[kernel_test]
fn a_child_whose_budget_did_not_return_in_time_stays_removed() {
    let (composition, importer) = budget_scenario();

    let held = component_startup::open::<probe::Protocol>(
        &first_found::<probe::Protocol>(&importer, HELD),
        BUDGET_WAIT,
    )
    .map(|answer| answer.map(drop));
    let pinged = ping(&importer, PROBE);

    kernel_tests::kassert_eq!(held, Ok(Err(SessionError::Unavailable)));
    kernel_tests::kassert_eq!(pinged, Ok(PING));
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

    let found = await_found(&importer, STARVED, 1);
    let found_pinged = found.first().map(ping_found);
    let pinged = ping(&importer, PROBE);

    kernel_tests::kassert_eq!(found.len(), 1);
    kernel_tests::kassert_eq!(found_pinged, Some(Ok(PING)));
    kernel_tests::kassert_eq!(pinged, Ok(PING));
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
fn a_device_outside_the_platform_description_is_not_described() {
    let composition = scenario(&[PLATFORM]);
    let importer = composition.importer(&[platform_of(PLATFORM_REQUIREMENT)]);
    let session = platform_session(&importer, PLATFORM_REQUIREMENT);

    let absent_line = line(&session, ABSENT).map(drop);
    let absent_window = window(&session, ABSENT).map(drop);
    let relative_line = line(&session, RELATIVE).map(drop);

    kernel_tests::kassert_eq!(absent_line, Err(PlatformError::NotDescribed));
    kernel_tests::kassert_eq!(absent_window, Err(PlatformError::NotDescribed));
    kernel_tests::kassert_eq!(relative_line, Err(PlatformError::NotDescribed));
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
    let held = window(&first, PL031).expect("first session takes the region");

    let contested_line = line(&second, PL031).map(drop);
    let contested_window = window(&second, PL031).map(drop);
    drop(first);
    let region_revoked = await_revoked(|| held.region.inspect());
    let line_revoked = await_revoked(|| held_line.ack());
    let granted_line = line(&second, PL031).map(drop);
    let granted_window = window(&second, PL031).map(drop);

    kernel_tests::kassert_eq!(contested_line, Err(PlatformError::Taken));
    kernel_tests::kassert_eq!(contested_window, Err(PlatformError::Taken));
    kernel_tests::kassert!(region_revoked);
    kernel_tests::kassert!(line_revoked);
    kernel_tests::kassert_eq!(granted_line, Ok(()));
    kernel_tests::kassert_eq!(granted_window, Ok(()));
    drop(second);
    finish(composition);
}

#[kernel_test]
fn a_window_in_the_region_of_another_session_is_taken() {
    let composition = scenario(&[PLATFORM]);
    let importer = composition.importer(&[platform_of(PLATFORM_REQUIREMENT)]);
    let first = platform_session(&importer, PLATFORM_REQUIREMENT);
    let second = platform_session(&importer, PLATFORM_REQUIREMENT);
    let taken = window(&first, FIRST_VIRTIO).expect("first session takes the region");

    let contested = window(&second, SECOND_VIRTIO).map(drop);

    kernel_tests::kassert_eq!(contested, Err(PlatformError::Taken));
    drop((first, second, taken));
    finish(composition);
}

#[kernel_test]
fn windows_of_one_session_in_one_page_share_the_region_at_their_offsets_and_sizes() {
    let composition = scenario(&[PLATFORM]);
    let importer = composition.importer(&[platform_of(PLATFORM_REQUIREMENT)]);
    let session = platform_session(&importer, PLATFORM_REQUIREMENT);

    let first = window(&session, FIRST_VIRTIO).expect("first window");
    let second = window(&session, SECOND_VIRTIO).expect("second window");

    kernel_tests::kassert_eq!(region_base(&first.region), VIRTIO_REGION);
    kernel_tests::kassert_eq!(region_base(&second.region), VIRTIO_REGION);
    kernel_tests::kassert_eq!((first.offset, second.offset), (0, VIRTIO_STRIDE));
    kernel_tests::kassert_eq!((first.size, second.size), (VIRTIO_STRIDE, VIRTIO_STRIDE));
    kernel_tests::kassert!(
        first.region.inspect().expect("region inspects").size_bytes > VIRTIO_STRIDE
    );
    drop(session);
    finish(composition);
}

#[kernel_test]
fn the_grants_of_a_platform_incarnation_are_revoked_when_it_ends() {
    let composition = scenario(&[PLATFORM]);
    let importer = composition.importer(&[platform_of(PLATFORM_REQUIREMENT)]);
    let session = platform_session(&importer, PLATFORM_REQUIREMENT);
    let held_line = line(&session, PL031).expect("session takes the line");
    let held = window(&session, PL031).expect("session takes the region");

    composition.terminate(PLATFORM.name, TERMINATE_EXIT_CODE);
    let region_revoked = await_revoked(|| held.region.inspect());
    let line_revoked = await_revoked(|| held_line.ack());
    let renewed = platform_session(&importer, PLATFORM_REQUIREMENT);
    let renewed_line = line(&renewed, PL031).map(drop);
    let renewed_window = window(&renewed, PL031).map(drop);

    kernel_tests::kassert!(region_revoked);
    kernel_tests::kassert!(line_revoked);
    kernel_tests::kassert_eq!(renewed_line, Ok(()));
    kernel_tests::kassert_eq!(renewed_window, Ok(()));
    drop((session, renewed));
    finish(composition);
}

#[kernel_test]
fn sessions_read_one_description_through_read_only_copies() {
    let composition = scenario(&[PLATFORM]);
    let importer = composition.importer(&[platform_of(PLATFORM_REQUIREMENT)]);
    let first = platform_session(&importer, PLATFORM_REQUIREMENT);
    let second = platform_session(&importer, PLATFORM_REQUIREMENT);

    let (region, offset) = description(&first).expect("first description");
    let (repeated, repeated_offset) = description(&first).expect("repeated description");
    let (other, other_offset) = description(&second).expect("second description");
    let writable = region.duplicate(Rights::WRITE, None).map(drop);
    let mapping = region
        .map_full_read_only()
        .expect("description maps read-only");
    let tree = Description::new(mapping.as_bytes(), offset).expect("description parses");

    kernel_tests::kassert_eq!(
        (region_base(&repeated), repeated_offset),
        (region_base(&region), offset)
    );
    kernel_tests::kassert_eq!(
        (region_base(&other), other_offset),
        (region_base(&region), offset)
    );
    kernel_tests::kassert_eq!(writable, Err(Error::Syscall(SyscallError::AccessDenied)));
    kernel_tests::kassert!(tree.find(PL031).is_some());
    drop((first, second));
    finish(composition);
}

#[kernel_test]
fn the_description_and_its_copies_are_revoked_when_the_session_closes() {
    let composition = scenario(&[PLATFORM]);
    let importer = composition.importer(&[platform_of(PLATFORM_REQUIREMENT)]);
    let first = platform_session(&importer, PLATFORM_REQUIREMENT);
    let second = platform_session(&importer, PLATFORM_REQUIREMENT);
    let (region, _) = description(&first).expect("first description");
    let copy = region
        .duplicate(Rights::READ, None)
        .expect("description copies");

    drop(first);
    let region_revoked = await_revoked(|| region.inspect());
    let copy_revoked = await_revoked(|| copy.inspect());
    let renewed = description(&second).map(drop);

    kernel_tests::kassert!(region_revoked);
    kernel_tests::kassert!(copy_revoked);
    kernel_tests::kassert_eq!(renewed, Ok(()));
    drop(second);
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
    let granted = window.inspect().is_ok();

    composition.terminate(IMAGE_FS.name, TERMINATE_EXIT_CODE);
    let revoked = await_revoked(|| window.inspect());

    kernel_tests::kassert!(granted);
    kernel_tests::kassert!(revoked);
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

    let lines = await_component_lines(SCOPE_CONSUMER.name, 2);

    kernel_tests::kassert_eq!(lines, ["area read", "area read"]);
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
    let restarted = resolved(&filesystem, "/");
    let rewritten = written(&restarted);

    kernel_tests::kassert!(renamed.is_err());
    kernel_tests::kassert_eq!(rewritten, Err(FileSystemError::NotFound));
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
    let restarted = await_component_lines(RAM_FS.name, 2);
    let rewritten = written(&resolved(&tmp, "/"));

    kernel_tests::kassert_eq!(restarted, ["started", "started"]);
    kernel_tests::kassert_eq!(rewritten, Err(FileSystemError::NotFound));
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
    let mounted = resolution(&before, IMAGE_MOUNT, VIEW_WAIT);

    composition.terminate(ROUTER_NAME, TERMINATE_EXIT_CODE);
    let renewed = [(ROOT_VIEW, IMAGE_MOUNT), (TMP_VIEW, "/")].map(|(requirement, text)| {
        let port = importer.port::<namespace::Protocol>(requirement);
        resolution(&port, text, VIEW_WAIT)
    });
    let stale = resolution(&before, IMAGE_MOUNT, SHORT_WAIT);

    kernel_tests::kassert_eq!(mounted, Ok(Ok(())));
    kernel_tests::kassert_eq!(renewed, [Ok(Ok(())), Ok(Ok(()))]);
    kernel_tests::kassert!(stale.is_err());
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
    let renewed = written(&tmp);
    let kept = written(&files);

    kernel_tests::kassert_eq!(renewed, Ok(NodeKind::File));
    kernel_tests::kassert_eq!(kept, Ok(NodeKind::File));
    finish(composition);
}

#[kernel_test]
fn a_removed_file_system_frees_its_prefix() {
    let composition = scenario(&[ROUTER, IMAGE_FS, NESTED_SPARE_FS]);
    let importer =
        composition.importer(&[view(ROOT_VIEW, ""), view(SPARE_VIEW, NESTED_SPARE_MOUNT)]);
    let spare = importer.port::<namespace::Protocol>(SPARE_VIEW);
    let mounted = resolution(&spare, "/", VIEW_WAIT);

    composition.terminate(NESTED_SPARE_FS.name, TERMINATE_EXIT_CODE);
    let freed = resolution(&spare, "/", OUTSIDE_WAIT);
    let root = importer.port::<namespace::Protocol>(ROOT_VIEW);
    let nested = resolution(&root, NESTED_SPARE_PATH, VIEW_WAIT);

    kernel_tests::kassert_eq!(mounted, Ok(Ok(())));
    kernel_tests::kassert_eq!(freed, Err(IpcError::Timeout));
    kernel_tests::kassert_eq!(nested, Ok(Err(FileSystemError::NotFound)));
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
    let freed = resolution(&other, "/", OUTSIDE_WAIT);
    let ended = mounted.ended(CALL_WAIT);

    kernel_tests::kassert_eq!(freed, Err(IpcError::Timeout));
    kernel_tests::kassert!(ended);
    drop(mounted);
    finish(composition);
}

#[kernel_test]
fn optional_and_all_start_with_empty_or_populated_provider_lists() {
    let consumer = ComponentSpec {
        ..fixture("/multiplicity-fixture", "multiplicity")
    };

    let lines = [&[][..], &[1][..]].map(|providers| {
        let composition = if providers.is_empty() {
            scenario(&[consumer])
        } else {
            scenario(&[EXPORT, consumer])
        };
        let lines = await_component_lines(consumer.name, 5);
        finish(composition);
        lines
    });

    kernel_tests::kassert_eq!(
        lines,
        [
            [
                "optional empty",
                "started",
                "providers 0",
                "all refused",
                "image found"
            ],
            [
                "optional empty",
                "started",
                "providers 1",
                "all refused",
                "image found"
            ]
        ]
    );
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
    let started = await_component_lines(consumer.name, 5);
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
    let lines = await_component_lines(consumer.name, 6);

    kernel_tests::kassert_eq!(
        started,
        [
            "optional empty",
            "started",
            "providers 0",
            "all refused",
            "image found"
        ]
    );
    kernel_tests::kassert_eq!(
        lines,
        [
            "optional empty",
            "started",
            "providers 0",
            "all refused",
            "image found",
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
    let pinged = ping(&importer, PROBE);
    let before = enumerated(&importer, PROBE);
    let held = first_found::<probe::Protocol>(&importer, PROBE);
    let record = composition
        .try_add(
            component(program),
            component_scenario_session(badge).expect("log"),
        )
        .expect("same name can register after refusal");
    let found = importer.found::<probe::Protocol>(PROBE);
    let found_pinged = found.get(1).map(ping_found);
    let drained_count = drained(&before).len();
    let held_pinged = ping_found(&held);

    kernel_tests::kassert!(matches!(failure, Err(rootkeeper::AddFailure::Declaration)));
    kernel_tests::kassert_eq!(pinged, Ok(PING));
    kernel_tests::kassert_eq!(record, 2);
    kernel_tests::kassert_eq!(found.len(), 2);
    kernel_tests::kassert_eq!(found_pinged, Some(Ok(PING)));
    kernel_tests::kassert_eq!(drained_count, 1);
    kernel_tests::kassert_eq!(held_pinged, Ok(PING));
    finish(composition);
}

#[kernel_test]
fn two_instances_of_one_program_keep_their_mounts_separate() {
    let composition = scenario(&[ROUTER, RAM_FS, SPARE_FS]);
    let importer = composition.importer(&[view(TMP_VIEW, "/tmp"), view(SPARE_VIEW, "/spare")]);
    let first = resolved(&importer.port::<namespace::Protocol>(TMP_VIEW), "/");
    let second = resolved(&importer.port::<namespace::Protocol>(SPARE_VIEW), "/");

    write(&first);
    let first_written = written(&first);
    let second_written = written(&second);

    kernel_tests::kassert_eq!(first_written, Ok(NodeKind::File));
    kernel_tests::kassert_eq!(second_written, Err(FileSystemError::NotFound));
    finish(composition);
}

#[kernel_test]
fn a_requirement_without_providers_enumerates_nothing() {
    let composition = scenario(&[]);
    let importer = composition.importer(&[probe_of(PROBE)]);

    let found = importer.found::<probe::Protocol>(PROBE);

    kernel_tests::kassert!(found.is_empty());
    finish(composition);
}

#[kernel_test]
fn an_enumeration_by_a_session_requirement_is_refused() {
    let composition = scenario(&[EXPORT]);
    let importer = composition.importer(&[crate::composition::Import {
        declaration: RequirementDeclaration::session(PROBE, probe::Protocol::ID),
        namespace_root: None,
    }]);

    let answer = importer
        .enumeration::<probe::Protocol>(PROBE, Attributes::empty())
        .map(|answer| answer.map(drop));
    let pinged = ping(&importer, PROBE);

    kernel_tests::kassert_eq!(answer, Ok(Err(SessionError::Refused)));
    kernel_tests::kassert_eq!(pinged, Ok(PING));
    finish(composition);
}

#[kernel_test]
fn a_session_with_two_matching_exports_is_refused() {
    let composition = scenario(&[EXPORT, NEIGHBOUR]);
    let importer = composition.importer(&[probe_of(PROBE)]);

    let answer = requested::<probe::Protocol>(&importer, PROBE, CALL_WAIT);
    let found_count = importer.found::<probe::Protocol>(PROBE).len();

    kernel_tests::kassert_eq!(answer, Ok(Err(SessionError::Refused)));
    kernel_tests::kassert_eq!(found_count, 2);
    finish(composition);
}

#[kernel_test]
fn a_found_export_opens_only_as_its_protocol() {
    let composition = scenario(&[EXPORT]);
    let importer = composition.importer(&[probe_of(PROBE)]);
    let found = first_found::<probe::Protocol>(&importer, PROBE);

    let other = component_startup::open::<rtc::Protocol>(&found, STARTUP_REQUEST_TIMEOUT)
        .map(|answer| answer.map(drop));
    let all = importer
        .enumeration::<rtc::Protocol>(PROBE, Attributes::empty())
        .map(|answer| answer.map(drop));
    let pinged = ping_found(&found);

    kernel_tests::kassert_eq!(other, Ok(Err(SessionError::Refused)));
    kernel_tests::kassert_eq!(all, Ok(Err(SessionError::Refused)));
    kernel_tests::kassert_eq!(pinged, Ok(PING));
    finish(composition);
}

#[kernel_test]
fn next_skips_an_export_removed_after_all() {
    let composition = scenario(&[EXPORT, NEIGHBOUR]);
    let importer = composition.importer(&[probe_of(PROBE)]);
    let started = await_component_lines(EXPORT.name, 1);
    let enumeration = enumerated(&importer, PROBE);

    composition.terminate(EXPORT.name, 0);
    let remaining = await_found(&importer, PROBE, 1).len();
    let rest = drained(&enumeration);
    let rest_pinged = rest.first().map(ping_found);

    kernel_tests::kassert_eq!(started, ["started"]);
    kernel_tests::kassert_eq!(remaining, 1);
    kernel_tests::kassert_eq!(rest.len(), 1);
    kernel_tests::kassert_eq!(rest_pinged, Some(Ok(PING)));
    finish(composition);
}

#[kernel_test]
fn a_filtered_export_answers_with_the_first_matching_filter() {
    let composition = scenario(&[EXPORT, FILTERED]);
    let importer = composition.importer(&[probe_of(PROBE)]);

    let unfiltered = filters_found(&importer, &[]);
    let first_matching = filters_found(&importer, &[pair("compatible", "a"), pair("role", "c")]);
    let unmatched = filters_found(&importer, &[pair("compatible", "b")]);
    let with_extra = filters_found(
        &importer,
        &[
            pair("compatible", "b"),
            pair("role", "c"),
            pair("extra", "d"),
        ],
    );
    let role_only = filters_found(&importer, &[pair("role", "c")]);
    let bytes = query(&[pair("role", "c")]);
    let parent =
        startup_contract::Client::new(PortTransport::new(importer.handle())).with_wait(CALL_WAIT);
    let filtered = session::<probe::Protocol>(
        &parent,
        PROBE,
        Attributes::decode(&bytes).expect("query decodes"),
    )
    .expect("session call")
    .expect("one export matches")
    .client()
    .with_wait(CALL_WAIT)
    .ping(PING);

    kernel_tests::kassert_eq!(unfiltered, [""]);
    kernel_tests::kassert_eq!(first_matching, ["compatible=a"]);
    kernel_tests::kassert!(unmatched.is_empty());
    kernel_tests::kassert_eq!(with_extra, ["compatible=b,role=c"]);
    kernel_tests::kassert_eq!(role_only, ["role=c"]);
    kernel_tests::kassert_eq!(filtered, Ok(PING));
    finish(composition);
}

#[kernel_test]
fn a_query_with_a_repeated_key_is_refused_and_the_next_one_is_served() {
    let composition = scenario(&[EXPORT]);
    let importer = composition.importer(&[probe_of(PROBE)]);
    let bytes = query(&[pair("role", "c"), pair("role", "c")]);
    let requirement = SessionRequirement {
        name: Str::new(PROBE).expect("requirement fits"),
        protocol: Str::new(probe::Protocol::ID).expect("protocol fits"),
    };

    let answer = startup_contract::Client::new(PortTransport::new(importer.handle()))
        .with_wait(CALL_WAIT)
        .session(requirement, Bytes::new(&bytes).expect("query fits"))
        .map(|answer| answer.map(drop));
    let pinged = ping(&importer, PROBE);

    kernel_tests::kassert_eq!(answer, Ok(Err(SessionError::Refused)));
    kernel_tests::kassert_eq!(pinged, Ok(PING));
    finish(composition);
}

#[kernel_test]
fn an_importer_finds_a_root_export_but_cannot_open_it() {
    let composition = scenario(&[]);
    let importer = composition.importer(&[crate::composition::Import {
        declaration: RequirementDeclaration::all(HARDWARE_REQUIREMENT, hardware::Protocol::ID),
        namespace_root: None,
    }]);

    let found = importer.found::<hardware::Protocol>(HARDWARE_REQUIREMENT);
    let opened = found.first().map(|found| {
        component_startup::open::<hardware::Protocol>(found, STARTUP_REQUEST_TIMEOUT)
            .map(|answer| answer.map(drop))
    });
    let direct = requested::<hardware::Protocol>(&importer, HARDWARE_REQUIREMENT, CALL_WAIT);

    kernel_tests::kassert_eq!(found.len(), 1);
    kernel_tests::kassert_eq!(opened, Some(Ok(Err(SessionError::Refused))));
    kernel_tests::kassert_eq!(direct, Ok(Err(SessionError::Refused)));
    finish(composition);
}

#[kernel_test]
fn a_finished_enumeration_keeps_the_results_it_gave() {
    let composition = scenario(&[EXPORT, NEIGHBOUR]);
    let importer = composition.importer(&[probe_of(PROBE)]);
    let enumeration = enumerated(&importer, PROBE);

    let found = drained(&enumeration);
    let after = component_startup::next(&enumeration, STARTUP_REQUEST_TIMEOUT)
        .map(|answer| answer.map(|next| next.is_none()));
    drop(enumeration);
    let pinged = found.iter().all(|found| ping_found(found) == Ok(PING));

    kernel_tests::kassert_eq!(after, Ok(Ok(true)));
    kernel_tests::kassert_eq!(found.len(), 2);
    kernel_tests::kassert!(pinged);
    finish(composition);
}

#[kernel_test]
fn closing_one_of_two_enumerations_leaves_the_other() {
    let composition = scenario(&[EXPORT, NEIGHBOUR]);
    let importer = composition.importer(&[probe_of(PROBE)]);
    let first = enumerated(&importer, PROBE);
    let second = enumerated(&importer, PROBE);

    drop(first);
    let rest_count = drained(&second).len();

    kernel_tests::kassert_eq!(rest_count, 2);
    finish(composition);
}

#[kernel_test]
fn closing_one_result_leaves_the_others_of_any_importer() {
    let composition = scenario(&[EXPORT]);
    let importer = composition.importer(&[probe_of(PROBE)]);
    let other = composition.importer(&[probe_of(PROBE)]);
    let first = first_found::<probe::Protocol>(&importer, PROBE);
    let second = first_found::<probe::Protocol>(&importer, PROBE);
    let foreign = first_found::<probe::Protocol>(&other, PROBE);

    drop(first);
    let sibling_closed = ping_found(&second);
    drop(foreign);
    drop(other);
    let importer_closed = ping_found(&second);

    kernel_tests::kassert_eq!(sibling_closed, Ok(PING));
    kernel_tests::kassert_eq!(importer_closed, Ok(PING));
    finish(composition);
}

#[kernel_test]
fn closing_an_importer_revokes_its_searches_and_keeps_foreign_results() {
    let composition = scenario(&[EXPORT]);
    let importer = composition.importer(&[probe_of(PROBE)]);
    let other = composition.importer(&[probe_of(PROBE)]);
    let enumeration = enumerated(&importer, PROBE);
    let found = first_found::<probe::Protocol>(&importer, PROBE);
    let foreign = first_found::<probe::Protocol>(&other, PROBE);

    drop(importer);
    let enumeration_revoked = await_revoked(|| enumeration.handle().duplicate(Rights::WRITE, None));
    let found_revoked = await_revoked(|| found.session.handle().duplicate(Rights::WRITE, None));
    let foreign_pinged = ping_found(&foreign);

    kernel_tests::kassert!(enumeration_revoked);
    kernel_tests::kassert!(found_revoked);
    kernel_tests::kassert_eq!(foreign_pinged, Ok(PING));
    finish(composition);
}

#[kernel_test]
fn repeated_searches_release_their_grants() {
    let composition = scenario(&[EXPORT, NEIGHBOUR]);
    let importer = composition.importer(&[probe_of(PROBE)]);

    let pinged = (0..SEARCH_CYCLES).all(|_| {
        let enumeration = enumerated(&importer, PROBE);
        let found = component_startup::next(&enumeration, STARTUP_REQUEST_TIMEOUT)
            .expect("next call")
            .expect("next answered")
            .expect("a provider is found");
        ping_found(&found) == Ok(PING)
    });
    let found_count = importer.found::<probe::Protocol>(PROBE).len();

    kernel_tests::kassert!(pinged);
    kernel_tests::kassert_eq!(found_count, 2);
    finish(composition);
}
