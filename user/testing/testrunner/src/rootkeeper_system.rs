//! Требования, журнал, отказы, перезапуски, бюджет, устройства и файловые системы записей
//! композиции rootkeeper.

use alloc::{string::String, vec::Vec};

use component_abi::{
    AcceptError, LOG_REQUIREMENT, ResourceError, SessionError,
    component_startup as startup_contract,
};
use component_startup::{accept, session};
use filesystem_abi::{
    AccessMode, FileSystemError, NodeKind, OpenFlags, file_system, namespace, namespace_admin,
};
use ipc::{
    Protocol,
    wire::{IpcError, Str},
};
use kernel_objects::{Error, IrqLine, MemoryRegion, Port, Session, SessionTransport};
use kernel_tests::kernel_test;
use log_abi::log;
use log_relay::SourceNames as _;
use rootkeeper::{
    BootFailure, ImporterRequirement, Stopped,
    relay::{ComponentNames, log_badge},
};
use rtc_abi::rtc;
use runtime::{JoinHandle, PortTransport, Priority, Timeout, spawn_owned};
use syscall::SyscallError;
use userland_image::{
    ComponentSpec, DeviceSpec, PlanDecodeError, RequirementSource, RequirementSourceSpec,
    RequirementSpec, USERLAND_PROGRAM_MAGIC,
};

use crate::{
    composition::{
        BOOT_PLAN_ENTRY, Composition, Importer, Mounted, ROUTER_NAME, admin, encoded, image, pause,
        request, view,
    },
    device_forwarding_protocol::device_forwarding,
    filesystem::{connect, open, path},
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
const RAMFS: &str = "ramfs";
const ROOT_VIEW: &str = "root";
const NAMESPACE: &str = "namespace";
const RTC_REQUIREMENT: &str = "rtc";
const LINE: &str = "line";
const WINDOW: &str = "window";
const FIRST_WINDOW: &str = "first";
const SECOND_WINDOW: &str = "second";
const IMAGE: &str = "image";
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
const AREA_SCOPE: &str = "/scope-consumer-fixture";

const PROBE_PROVIDER: u16 = 0;
const ROUTER_RECORD: u16 = 0;
const RAM_FS_RECORD: u16 = 1;
const IMAGE_FS_RECORD: u16 = 2;

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

const PROBE_EXPORTS: &[&str] = &[probe::Protocol::ID];
const FILE_SYSTEM_EXPORTS: &[&str] = &[file_system::Protocol::ID];
const ROUTER_EXPORTS: &[&str] = &[namespace::Protocol::ID, namespace_admin::Protocol::ID];
const FORWARDING_EXPORTS: &[&str] = &[device_forwarding::Protocol::ID];

const PL031: &[&str] = &["arm,pl031"];
const ABSENT_CLOCK: &[&str] = &["acme,absent-clock"];
const GPIO: &[&str] = &["arm,pl061"];
const VIRTIO: &[&str] = &["virtio,mmio"];

const RTC_DEVICES: &[RequirementSpec<'static>] =
    &device_requirements(device(PL031, 0), device(PL031, 0));
const ABSENT_DEVICES: &[RequirementSpec<'static>] =
    &device_requirements(device(ABSENT_CLOCK, 0), device(ABSENT_CLOCK, 0));
const SECOND_INSTANCE_DEVICES: &[RequirementSpec<'static>] =
    &device_requirements(device(PL031, 1), device(PL031, 0));
const GPIO_LINE_DEVICES: &[RequirementSpec<'static>] =
    &device_requirements(device(GPIO, 0), device(PL031, 0));
const FIRST_VIRTIO_DEVICES: &[RequirementSpec<'static>] =
    &device_requirements(device(VIRTIO, 0), device(VIRTIO, 0));
const SECOND_VIRTIO_DEVICES: &[RequirementSpec<'static>] =
    &device_requirements(device(VIRTIO, 1), device(VIRTIO, 1));
const VIRTIO_PAIR: &[RequirementSpec<'static>] = &[
    RequirementSpec {
        name: FIRST_WINDOW,
        source: RequirementSourceSpec::Mmio(device(VIRTIO, 0)),
    },
    RequirementSpec {
        name: SECOND_WINDOW,
        source: RequirementSourceSpec::Mmio(device(VIRTIO, 1)),
    },
];
const RTC_LINE: &[RequirementSpec<'static>] = &[RequirementSpec {
    name: LINE,
    source: RequirementSourceSpec::Irq(device(PL031, 0)),
}];

const RAM_FS_REQUIREMENTS: &[RequirementSpec<'static>] = &[namespace_requirement("/tmp")];
const SPARE_FS_REQUIREMENTS: &[RequirementSpec<'static>] = &[namespace_requirement("/spare")];
const NESTED_SPARE_FS_REQUIREMENTS: &[RequirementSpec<'static>] =
    &[namespace_requirement(NESTED_SPARE_MOUNT)];
const CRASH_FS_REQUIREMENTS: &[RequirementSpec<'static>] = &[namespace_requirement("/crash")];

const IMAGE_FS_REQUIREMENTS: &[RequirementSpec<'static>] = &[
    namespace_requirement(IMAGE_MOUNT),
    RequirementSpec {
        name: IMAGE,
        source: RequirementSourceSpec::Region,
    },
];

const AREA_REQUIREMENTS: &[RequirementSpec<'static>] = &[RequirementSpec {
    name: AREA,
    source: RequirementSourceSpec::Export {
        provider: IMAGE_FS_RECORD,
        protocol: file_system::Protocol::ID,
        scope: AREA_SCOPE,
    },
}];

const HOLDER_REQUIREMENTS: &[RequirementSpec<'static>] = &[RequirementSpec {
    name: FILES,
    source: RequirementSourceSpec::Export {
        provider: RAM_FS_RECORD,
        protocol: file_system::Protocol::ID,
        scope: "",
    },
}];

const PROBE_REQUIREMENTS: &[RequirementSpec<'static>] = &[RequirementSpec {
    name: PROBE,
    source: RequirementSourceSpec::Export {
        provider: PROBE_PROVIDER,
        protocol: probe::Protocol::ID,
        scope: "",
    },
}];

const EXPORT: ComponentSpec<'static> = fixture("/export-fixture", "export-fixture");
const EXPORT_ENTRY: &str = "/export-fixture/program";

const IDLE_EXPORT: ComponentSpec<'static> = fixture("/idle-export-fixture", "idle-export-fixture");

const REFUSAL_PROBE: ComponentSpec<'static> = ComponentSpec {
    requirements: PROBE_REQUIREMENTS,
    exports: &[],
    ..fixture("/refusal-probe-fixture", "refusal-probe-fixture")
};

const LOG_DUPLICATE: ComponentSpec<'static> = ComponentSpec {
    exports: &[],
    ..fixture("/log-duplicate-fixture", "log-duplicate-fixture")
};

const NEIGHBOUR: ComponentSpec<'static> = ComponentSpec {
    name: "neighbour",
    ..EXPORT
};

const FAILING: ComponentSpec<'static> = ComponentSpec {
    exports: &[],
    ..fixture("/failing-fixture", "failing-fixture")
};

const CLOSED: ComponentSpec<'static> = ComponentSpec {
    exports: &[],
    ..fixture("/managed-startup-closed", "managed-startup-closed")
};

const PROBE_IMPORTER: ComponentSpec<'static> = ComponentSpec {
    requirements: PROBE_REQUIREMENTS,
    exports: &[],
    ..fixture("/probe-importer-fixture", "probe-importer-fixture")
};

const HOLDER: ComponentSpec<'static> = ComponentSpec {
    requirements: HOLDER_REQUIREMENTS,
    memory_limit: 2 * MIB,
    restarts: 1,
    ..fixture("/budget-holder-fixture", "budget-holder-fixture")
};

const RTC: ComponentSpec<'static> = ComponentSpec {
    executable: "/driver-pl031",
    name: "driver-pl031",
    requirements: RTC_DEVICES,
    exports: &[rtc::Protocol::ID],
    memory_limit: MIB,
    restarts: 0,
};

const FORWARDING: ComponentSpec<'static> = ComponentSpec {
    requirements: RTC_DEVICES,
    exports: FORWARDING_EXPORTS,
    ..fixture("/device-forwarding-fixture", "device-forwarding-fixture")
};

const IRQ: ComponentSpec<'static> = ComponentSpec {
    requirements: RTC_LINE,
    exports: &[],
    restarts: 1,
    ..fixture("/irq-fixture", "irq-fixture")
};

const MMIO_PAIR: ComponentSpec<'static> = ComponentSpec {
    requirements: VIRTIO_PAIR,
    exports: &[],
    ..fixture("/mmio-pair-fixture", "mmio-pair-fixture")
};

const ROUTER: ComponentSpec<'static> = ComponentSpec {
    executable: "/namespace-router",
    name: ROUTER_NAME,
    requirements: &[],
    exports: ROUTER_EXPORTS,
    memory_limit: 2 * MIB,
    restarts: 0,
};

const RAM_FS: ComponentSpec<'static> = ComponentSpec {
    executable: "/filesystem-ramfs",
    name: "filesystem-ramfs",
    requirements: RAM_FS_REQUIREMENTS,
    exports: FILE_SYSTEM_EXPORTS,
    memory_limit: 4 * MIB,
    restarts: 0,
};

const IMAGE_FS: ComponentSpec<'static> = ComponentSpec {
    executable: "/filesystem-imagefs",
    name: "filesystem-imagefs",
    requirements: IMAGE_FS_REQUIREMENTS,
    ..RAM_FS
};

const SPARE_FS: ComponentSpec<'static> = ComponentSpec {
    executable: "/filesystem-ramfs",
    name: "spare-filesystem",
    requirements: SPARE_FS_REQUIREMENTS,
    ..RAM_FS
};

const NESTED_SPARE_FS: ComponentSpec<'static> = ComponentSpec {
    requirements: NESTED_SPARE_FS_REQUIREMENTS,
    ..SPARE_FS
};

const CRASH_FS: ComponentSpec<'static> = ComponentSpec {
    executable: "/crash-filesystem",
    name: "crash-filesystem",
    requirements: CRASH_FS_REQUIREMENTS,
    restarts: 1,
    ..RAM_FS
};

const SCOPE_CONSUMER: ComponentSpec<'static> = ComponentSpec {
    requirements: AREA_REQUIREMENTS,
    exports: &[],
    ..fixture("/scope-consumer-fixture", "scope-consumer-fixture")
};

const fn fixture(executable: &'static str, name: &'static str) -> ComponentSpec<'static> {
    ComponentSpec {
        executable,
        name,
        requirements: &[],
        exports: PROBE_EXPORTS,
        memory_limit: MIB,
        restarts: 0,
    }
}

const fn device(compatible: &'static [&'static str], instance: u16) -> DeviceSpec<'static> {
    DeviceSpec {
        compatible,
        instance,
        resource: 0,
    }
}

const fn device_requirements(
    line: DeviceSpec<'static>,
    window: DeviceSpec<'static>,
) -> [RequirementSpec<'static>; 2] {
    [
        RequirementSpec {
            name: LINE,
            source: RequirementSourceSpec::Irq(line),
        },
        RequirementSpec {
            name: WINDOW,
            source: RequirementSourceSpec::Mmio(window),
        },
    ]
}

const fn namespace_requirement(scope: &'static str) -> RequirementSpec<'static> {
    RequirementSpec {
        name: NAMESPACE,
        source: RequirementSourceSpec::Export {
            provider: ROUTER_RECORD,
            protocol: namespace_admin::Protocol::ID,
            scope,
        },
    }
}

const fn probe_of(name: &'static str, provider: &'static str) -> ImporterRequirement<'static> {
    ImporterRequirement {
        name,
        provider,
        protocol: probe::Protocol::ID,
        scope: "",
    }
}

const fn file_system_of(
    name: &'static str,
    provider: &'static str,
) -> ImporterRequirement<'static> {
    ImporterRequirement {
        name,
        provider,
        protocol: file_system::Protocol::ID,
        scope: "",
    }
}

fn scenario(components: &[ComponentSpec<'_>]) -> Composition {
    let plan = encoded(components);
    start_component_relay(ComponentNames::of(&plan));
    Composition::start(plan, component_scenario_session)
}

fn finish(composition: Composition) {
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

const fn forwarding_of(name: &'static str) -> ImporterRequirement<'static> {
    ImporterRequirement {
        name,
        provider: name,
        protocol: device_forwarding::Protocol::ID,
        scope: "",
    }
}

fn requested<P: Protocol>(
    importer: &Importer,
    requirement: &str,
    wait: Timeout,
) -> Result<Result<(), SessionError>, IpcError> {
    request::<P>(importer.handle(), requirement, wait).map(|answer| answer.map(drop))
}

fn forwarding(
    importer: &Importer,
    requirement: &str,
) -> Result<Result<(), SessionError>, IpcError> {
    requested::<device_forwarding::Protocol>(importer, requirement, CALL_WAIT)
}

fn await_revoked(window: &MemoryRegion) -> bool {
    (0..REVOKE_POLLS).any(|_| {
        let revoked = window.inspect().err() == Some(Error::Syscall(SyscallError::Revoked));
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
    let importer = composition.importer(&[
        probe_of(HELD, HOLDER.name),
        probe_of(PROBE, NEIGHBOUR.name),
        file_system_of(RAMFS, RAM_FS.name),
    ]);
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
    let lines = await_component_lines(REFUSAL_PROBE.name, 6);
    finish(composition);
    lines
}

#[kernel_test]
fn a_log_badge_names_its_record() {
    let plan = encoded(&[EXPORT, IDLE_EXPORT]);
    let names = ComponentNames::of(&plan);

    for (record, component) in (0..).zip([EXPORT, IDLE_EXPORT]) {
        let name = names
            .name(log_badge(record).get())
            .expect("record badge is named");
        kernel_tests::kassert_eq!(name.as_str(), component.name);
    }
    kernel_tests::kassert!(names.name(log_badge(2).get()).is_none());
}

#[kernel_test]
fn compiled_plan_carries_the_boot_composition() {
    let plan = image()
        .plan(BOOT_PLAN_ENTRY)
        .expect("compiled plan decodes");
    let router = (0..plan.len())
        .find(|&record| {
            plan.component(record)
                .is_some_and(|component| component.name() == ROUTER_NAME)
        })
        .expect("plan carries the router");

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
        kernel_tests::kassert_eq!(
            component.requirement(NAMESPACE),
            Some(RequirementSource::Export {
                provider: router,
                protocol: namespace_admin::Protocol::ID,
                scope,
            })
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
fn a_requirement_asked_by_an_operation_of_another_kind_is_refused_at_once() {
    kernel_tests::kassert!(
        refusals()
            .iter()
            .any(|line| line == "kind Ok(Err(Refused))")
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
fn a_region_asked_on_a_session_requirement_is_refused_at_once() {
    kernel_tests::kassert!(
        refusals()
            .iter()
            .any(|line| line == "region Ok(Err(Refused))")
    );
}

#[kernel_test]
fn an_importer_cannot_take_the_requests_of_a_child_export() {
    let composition = scenario(&[EXPORT]);
    let importer = composition.importer(&[probe_of(PROBE, EXPORT.name)]);
    let parent = startup_contract::Client::new(PortTransport::new(importer.handle()))
        .with_wait(REFUSAL_WAIT);

    let taken = accept::<probe::Protocol>(&parent).map(|answer| answer.map(drop));
    let log = session::<log::Protocol>(&parent, LOG_REQUIREMENT).map(|answer| answer.map(drop));
    let region = parent
        .region(Str::new(PROBE).expect("requirement name fits"))
        .map(|answer| answer.map(drop));

    kernel_tests::kassert_eq!(taken, Ok(Err(AcceptError::Refused)));
    kernel_tests::kassert_eq!(log, Ok(Err(SessionError::Refused)));
    kernel_tests::kassert_eq!(region, Ok(Err(ResourceError::Refused)));
    kernel_tests::kassert_eq!(ping(&importer, PROBE), Ok(PING));
    finish(composition);
}

#[kernel_test]
fn an_importer_stays_waiting_while_its_provider_lives_without_accept() {
    let composition = scenario(&[IDLE_EXPORT]);
    let importer = composition.importer(&[probe_of(PROBE, IDLE_EXPORT.name)]);
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
    let importer = composition.importer(&[
        probe_of(HELD, IDLE_EXPORT.name),
        probe_of(PROBE, NEIGHBOUR.name),
    ]);
    kernel_tests::kassert_eq!(await_component_lines(IDLE_EXPORT.name, 1), ["started"]);
    let held = held_request(&importer, HELD, REQUEST_WAIT);
    pause(QUIET_WAIT);

    composition.terminate(IDLE_EXPORT.name, TERMINATE_EXIT_CODE);

    kernel_tests::kassert_eq!(answered(&held), UNAVAILABLE);
    let again = requested::<probe::Protocol>(&importer, HELD, CALL_WAIT);
    kernel_tests::kassert_eq!(again, Ok(Err(SessionError::Unavailable)));
    kernel_tests::kassert_eq!(ping(&importer, PROBE), Ok(PING));
    finish(composition);
}

#[kernel_test]
fn a_provider_that_exits_with_code_zero_leaves_its_importers_unavailable() {
    let composition = scenario(&[ComponentSpec {
        restarts: 2,
        ..EXPORT
    }]);
    let importer = composition.importer(&[probe_of(PROBE, EXPORT.name)]);
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
    let importer = composition.importer(&[probe_of(HELD, IDLE_EXPORT.name)]);
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
    let importer = composition.importer(&[view(ROOT_VIEW, ""), probe_of(PROBE, NEIGHBOUR.name)]);
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

    let served = requested::<probe::Protocol>(&importer, PROBE, QUIET_WAIT);
    kernel_tests::kassert_eq!(served, Ok(Ok(())));

    let removed = connect(importer.session::<file_system::Protocol>(RAMFS))
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
    kernel_tests::kassert_eq!(ping(&importer, PROBE), Ok(PING));
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
    let importer = composition.importer(&[
        probe_of(STARVED, EXPORT.name),
        probe_of(PROBE, NEIGHBOUR.name),
    ]);

    let answer = requested::<probe::Protocol>(&importer, STARVED, CALL_WAIT);

    kernel_tests::kassert_eq!(answer, Ok(Err(SessionError::Unavailable)));
    kernel_tests::kassert_eq!(ping(&importer, PROBE), Ok(PING));
    finish(composition);
}

#[kernel_test]
fn a_device_component_serves_time_over_a_minted_session() {
    let composition = scenario(&[RTC]);
    let importer = composition.importer(&[ImporterRequirement {
        name: RTC_REQUIREMENT,
        provider: RTC.name,
        protocol: rtc::Protocol::ID,
        scope: "",
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
fn a_restarted_child_takes_its_irq_line_again() {
    let composition = scenario(&[IRQ]);

    kernel_tests::kassert_eq!(
        await_component_lines(IRQ.name, 2),
        ["line taken", "line taken"]
    );
    finish(composition);
}

#[kernel_test]
fn a_restarted_child_takes_its_mmio_window_again() {
    let composition = scenario(&[ComponentSpec {
        restarts: 1,
        ..FORWARDING
    }]);
    let importer = composition.importer(&[forwarding_of(FORWARDING.name)]);

    let first = importer
        .session::<device_forwarding::Protocol>(FORWARDING.name)
        .client()
        .with_wait(CALL_WAIT)
        .forward()
        .expect("forward call");
    // Фикстура завершается после выдачи, и отзыв её окна означает уход первой incarnation.
    kernel_tests::kassert!(await_revoked(&MemoryRegion::from(first.window)));

    // Сессию выдаёт только перезапущенная incarnation, и она стартует лишь с окном MMIO.
    kernel_tests::kassert_eq!(forwarding(&importer, FORWARDING.name), Ok(Ok(())));
    finish(composition);
}

#[kernel_test]
fn a_device_outside_the_platform_description_is_refused_to_its_record() {
    let composition = scenario(&[
        ComponentSpec {
            requirements: ABSENT_DEVICES,
            ..FORWARDING
        },
        NEIGHBOUR,
    ]);
    let importer = composition.importer(&[
        forwarding_of(FORWARDING.name),
        probe_of(PROBE, NEIGHBOUR.name),
    ]);

    kernel_tests::kassert_eq!(
        forwarding(&importer, FORWARDING.name),
        Ok(Err(SessionError::Unavailable))
    );
    kernel_tests::kassert_eq!(ping(&importer, PROBE), Ok(PING));
    finish(composition);
}

#[kernel_test]
fn a_device_instance_outside_the_platform_is_refused_to_its_record() {
    let composition = scenario(&[
        ComponentSpec {
            requirements: SECOND_INSTANCE_DEVICES,
            ..FORWARDING
        },
        NEIGHBOUR,
    ]);
    let importer = composition.importer(&[
        forwarding_of(FORWARDING.name),
        probe_of(PROBE, NEIGHBOUR.name),
    ]);

    kernel_tests::kassert_eq!(
        forwarding(&importer, FORWARDING.name),
        Ok(Err(SessionError::Unavailable))
    );
    kernel_tests::kassert_eq!(ping(&importer, PROBE), Ok(PING));
    finish(composition);
}

#[kernel_test]
fn a_second_record_naming_a_taken_line_is_refused() {
    let first = ComponentSpec {
        name: "first-forwarding",
        ..FORWARDING
    };
    let second = ComponentSpec {
        name: "second-forwarding",
        ..FORWARDING
    };
    let composition = scenario(&[first, second]);
    let importer = composition.importer(&[forwarding_of(first.name), forwarding_of(second.name)]);

    let answers = [
        forwarding(&importer, first.name),
        forwarding(&importer, second.name),
    ];

    kernel_tests::kassert!(answers.contains(&Ok(Ok(()))));
    kernel_tests::kassert!(answers.contains(&Ok(Err(SessionError::Unavailable))));
    finish(composition);
}

#[kernel_test]
fn a_second_record_naming_a_taken_window_is_refused() {
    let gpio_line = ComponentSpec {
        name: "gpio-line-forwarding",
        requirements: GPIO_LINE_DEVICES,
        ..FORWARDING
    };
    let alone = scenario(&[gpio_line]);
    let importer = alone.importer(&[forwarding_of(gpio_line.name)]);
    kernel_tests::kassert_eq!(forwarding(&importer, gpio_line.name), Ok(Ok(())));
    finish(alone);

    let composition = scenario(&[FORWARDING, gpio_line]);
    let importer = composition.importer(&[
        forwarding_of(FORWARDING.name),
        forwarding_of(gpio_line.name),
    ]);
    let answers = [
        forwarding(&importer, FORWARDING.name),
        forwarding(&importer, gpio_line.name),
    ];

    kernel_tests::kassert!(answers.contains(&Ok(Ok(()))));
    kernel_tests::kassert!(answers.contains(&Ok(Err(SessionError::Unavailable))));
    finish(composition);
}

#[kernel_test]
fn a_second_record_naming_a_window_in_a_taken_page_is_refused() {
    let first = ComponentSpec {
        name: "first-virtio-forwarding",
        requirements: FIRST_VIRTIO_DEVICES,
        ..FORWARDING
    };
    let second = ComponentSpec {
        name: "second-virtio-forwarding",
        requirements: SECOND_VIRTIO_DEVICES,
        ..FORWARDING
    };
    for alone in [first, second] {
        let composition = scenario(&[alone]);
        let importer = composition.importer(&[forwarding_of(alone.name)]);
        kernel_tests::kassert_eq!(forwarding(&importer, alone.name), Ok(Ok(())));
        finish(composition);
    }

    let composition = scenario(&[first, second]);
    let importer = composition.importer(&[forwarding_of(first.name), forwarding_of(second.name)]);
    let answers = [
        forwarding(&importer, first.name),
        forwarding(&importer, second.name),
    ];

    kernel_tests::kassert!(answers.contains(&Ok(Ok(()))));
    kernel_tests::kassert!(answers.contains(&Ok(Err(SessionError::Unavailable))));
    finish(composition);
}

#[kernel_test]
fn windows_of_one_record_in_one_page_share_the_region_at_their_offsets() {
    let composition = scenario(&[MMIO_PAIR]);

    kernel_tests::kassert_eq!(
        await_component_lines(MMIO_PAIR.name, 1),
        ["regions 0xa000000 0xa000000 offsets 0x0 0x200"]
    );
    finish(composition);
}

#[kernel_test]
fn a_forwarded_device_grant_is_revoked_when_its_incarnation_ends() {
    let composition = scenario(&[FORWARDING]);
    let importer = composition.importer(&[forwarding_of(FORWARDING.name)]);

    let forwarded = importer
        .session::<device_forwarding::Protocol>(FORWARDING.name)
        .client()
        .with_wait(CALL_WAIT)
        .forward()
        .expect("forward call");
    let window = MemoryRegion::from(forwarded.window);
    let line = IrqLine::from(forwarded.line);

    kernel_tests::kassert!(await_revoked(&window));
    kernel_tests::kassert_eq!(
        line.ack().err(),
        Some(Error::Syscall(SyscallError::Revoked))
    );
    finish(composition);
}

#[kernel_test]
fn a_scoped_requirement_is_granted_anew_to_every_incarnation() {
    let composition = scenario(&[
        ROUTER,
        RAM_FS,
        IMAGE_FS,
        ComponentSpec {
            restarts: 1,
            ..SCOPE_CONSUMER
        },
    ]);

    kernel_tests::kassert_eq!(
        await_component_lines(SCOPE_CONSUMER.name, 2),
        ["area read", "area read"]
    );
    finish(composition);
}

#[kernel_test]
fn a_component_that_faults_is_restarted() {
    let composition = scenario(&[ROUTER, CRASH_FS]);
    let importer = composition.importer(&[file_system_of(FILES, CRASH_FS.name)]);
    let faulting = connect(importer.session::<file_system::Protocol>(FILES));
    write(&faulting);

    let renamed = faulting.rename(path(WRITTEN_PATH), path(RENAMED_PATH));

    kernel_tests::kassert!(renamed.is_err());
    let restarted = connect(importer.session::<file_system::Protocol>(FILES));
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
    let importer =
        composition.importer(&[file_system_of(FILES, RAM_FS.name), view(TMP_VIEW, "/tmp")]);
    let files = connect(importer.session::<file_system::Protocol>(FILES));
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
    let importer = composition.importer(&[
        view(ROOT_VIEW, ""),
        view(SPARE_VIEW, NESTED_SPARE_MOUNT),
        file_system_of(FILES, NESTED_SPARE_FS.name),
    ]);
    let spare = importer.port::<namespace::Protocol>(SPARE_VIEW);
    kernel_tests::kassert_eq!(resolution(&spare, "/", VIEW_WAIT), Ok(Ok(())));

    composition.terminate(NESTED_SPARE_FS.name, TERMINATE_EXIT_CODE);

    kernel_tests::kassert_eq!(
        requested::<file_system::Protocol>(&importer, FILES, CALL_WAIT),
        Ok(Err(SessionError::Unavailable))
    );
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
        admin("other-admin", "/other"),
        file_system_of(FILES, SPARE_FS.name),
    ]);
    let mounted = Mounted::new(
        &importer,
        OTHER_VIEW,
        importer.session::<file_system::Protocol>(FILES),
    );
    let other = importer.port::<namespace::Protocol>(OTHER_VIEW);

    composition.terminate(SPARE_FS.name, TERMINATE_EXIT_CODE);

    kernel_tests::kassert_eq!(
        requested::<file_system::Protocol>(&importer, FILES, CALL_WAIT),
        Ok(Err(SessionError::Unavailable))
    );
    kernel_tests::kassert_eq!(
        resolution(&other, "/", OUTSIDE_WAIT),
        Err(IpcError::Timeout)
    );
    kernel_tests::kassert!(mounted.ended(CALL_WAIT));
    drop(mounted);
    finish(composition);
}
