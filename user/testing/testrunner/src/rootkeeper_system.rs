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
use ipc::{CallError, Protocol, wire::IpcError};
use kernel_objects::{Error, IrqLine, MemoryRegion, Port, Session, SessionTransport};
use log_abi::{LogBuffer, log};
use log_relay::SourceNames as _;
use platform_abi::{Description, MmioWindow, PlatformError, platform};
use rootkeeper::{
    BootFailure, SEARCH_LIMITS, SearchLimits, Stopped,
    relay::{ComponentNames, LogSessions as _, LogSources, log_badge},
};
use rtc_abi::rtc;
use runtime::{JoinHandle, PortTransport, Timeout, spawn_owned};
use syscall::{Handle, ProcessName, Rights, SyscallError, kind};
use userland::USERLAND_PROGRAM_MAGIC;
use userland_image::{ComponentSpec, ExportIndex, PlanDecodeError};

use crate::{
    composition::{
        BOOT_PLAN_ENTRY, Composition, Importer, Mounted, ROUTER_NAME, admin, drained, encoded,
        image, pause, request, view,
    },
    filesystem::{IMAGE_FILE, connect, open},
    log_relay::{
        ScenarioLogSessions, await_component_lines, await_component_lines_with_prefix,
        await_component_sources, start_component_relay, stop_component_relay,
    },
    probe_protocol::{Pairs, probe},
};

const MIB: u64 = 1024 * 1024;

pub(crate) const PROBE: &str = "probe";
const HELD: &str = "held";
const STARVED: &str = "starved";
const FILES: &str = "files";
const ROOT_VIEW: &str = "root";
const RTC_REQUIREMENT: &str = "rtc";
pub(crate) const PLATFORM_REQUIREMENT: &str = "platform";
const AREA: &str = "area";
const SPARE_VIEW: &str = "spare";
const TMP_VIEW: &str = "tmp";
const OTHER_VIEW: &str = "other";
const BOOT_VIEW: &str = "boot";
const NESTED_VIEW: &str = "nested";
const HARDWARE_REQUIREMENT: &str = "hardware";
const FEATURED_REQUIREMENT: &str = "featured";

const HELD_PATH: &str = "/held";
const WRITTEN_PATH: &str = "/written";
const RENAMED_PATH: &str = "/renamed";
const IMAGE_MOUNT: &str = "/boot";
const NESTED_SPARE_MOUNT: &str = "/boot/spare";
const NESTED_SPARE_PATH: &str = "/boot/spare/x";
const AREA_SCOPE: &str = "/boot/scope-consumer-fixture";

const STOPPED_LINE: &str = "stopped: failed";
const DUPLICATE_REFUSED_LINES: [&str; 2] = ["Err(Syscall(AccessDenied))", STOPPED_LINE];
const BOOT_REFUSAL: &str = "mount table line 2 /boot: filesystem refused: Remote(Unbound)";
const TMP_MOUNT: &str = "mount /tmp";
const TMP_ENDED: &str = "mount /tmp ended";

pub(crate) const COMPONENT_REPLY_TIMEOUT: Timeout = Timeout::from_secs(2);
const PARENT_REFUSAL_TIMEOUT: Timeout = Timeout::from_secs(1);
pub(crate) const IDLE_PROVIDER_REQUEST_TIMEOUT: Timeout = Timeout::from_millis(500);
const STALE_VIEW_TIMEOUT: Timeout = Timeout::from_millis(500);
const REQUEST_DELIVERY_PAUSE: Timeout = Timeout::from_millis(200);
const SERVED_CHILD_REPLY_TIMEOUT: Timeout = Timeout::from_millis(200);
const REQUEST_ACROSS_EXIT_TIMEOUT: Timeout = Timeout::from_secs(3);
const BUDGET_RETURN_TIMEOUT: Timeout = Timeout::from_secs(8);
const HELD_REQUEST_TIMEOUT: Timeout = Timeout::from_secs(15);
const WATCH_WAIT_TIMEOUT: Timeout = Timeout::from_millis(300);
const RTC_REPLY_TIMEOUT: Timeout = Timeout::from_secs(15);
pub(crate) const POLL_INTERVAL: Timeout = Timeout::from_millis(10);
const RESOLVE_TIMEOUT: Timeout = Timeout::from_secs(5);
const FREED_PREFIX_TIMEOUT: Timeout = Timeout::from_millis(100);

pub(crate) const POLL_ATTEMPTS: usize = 300;
const SEARCH_CYCLES: usize = 300;

const TERMINATE_EXIT_CODE: u32 = 7;

const GRANTED: u32 = 0;
const UNAVAILABLE: u32 = 1;
const TIMED_OUT: u32 = 2;
const OTHER: u32 = 3;
const REFUSED: u32 = 4;

const PING: u64 = 0x5A;

const STARVED_MEMORY: u64 = 4096;

pub(crate) const PL031: &str = "/pl031@9010000";
pub(crate) const FIRST_VIRTIO: &str = "/virtio_mmio@a000000";
const SECOND_VIRTIO: &str = "/virtio_mmio@a000200";
const ABSENT: &str = "/absent@0";
const RELATIVE: &str = "pl031@9010000";

const VIRTIO_REGION: u64 = 0xa00_0000;
const VIRTIO_STRIDE: u64 = 0x200;
const VIRTIO_MAGIC: u32 = 0x7472_6976;

const PARENT: ComponentSpec<'static> = ComponentSpec {
    executable: "/testrunner",
    name: "testrunner",
    memory_limit: 0,
    restarts: 0,
};

pub(crate) const EXPORT: ComponentSpec<'static> = fixture("/export-fixture", "export-fixture");
const EXPORT_ENTRY: &str = "/export-fixture/exec";

pub(crate) const IDLE_EXPORT: ComponentSpec<'static> =
    fixture("/idle-export-fixture", "idle-export-fixture");

pub(crate) const FILTERED: ComponentSpec<'static> =
    fixture("/filtered-export-fixture", "filtered-export-fixture");

const REFUSAL_PROBE: ComponentSpec<'static> =
    fixture("/refusal-probe-fixture", "refusal-probe-fixture");

const LOG_DUPLICATE: ComponentSpec<'static> =
    fixture("/log-duplicate-fixture", "log-duplicate-fixture");

pub(crate) const NEIGHBOUR: ComponentSpec<'static> = ComponentSpec {
    name: "neighbour",
    ..EXPORT
};

const CLOSED: ComponentSpec<'static> = fixture("/managed-startup-closed", "managed-startup-closed");

const PROBE_IMPORTER: ComponentSpec<'static> =
    fixture("/probe-importer-fixture", "probe-importer-fixture");

const HOLDER: ComponentSpec<'static> = ComponentSpec {
    memory_limit: 2 * MIB,
    restarts: 1,
    ..fixture("/budget-holder-fixture", "budget-holder-fixture")
};

pub(crate) const PLATFORM: ComponentSpec<'static> = ComponentSpec {
    executable: "/platform",
    name: "platform",
    memory_limit: MIB,
    restarts: 1,
};

const CLOCK_DRIVERS: [(&str, &str); 2] = [
    ("arm,pl031", "driver-pl031"),
    ("qcom,pm8941-rtc", "driver-pm8941-rtc"),
];

const IRQ: ComponentSpec<'static> = ComponentSpec {
    restarts: 1,
    ..fixture("/irq-fixture", "irq-fixture")
};

const ROUTER: ComponentSpec<'static> = ComponentSpec {
    executable: "/mounts",
    name: ROUTER_NAME,
    memory_limit: 2 * MIB,
    restarts: 0,
};

const RAM_FS: ComponentSpec<'static> = ComponentSpec {
    executable: "/filesystem-ramfs",
    name: "filesystem-ramfs",
    memory_limit: 4 * MIB,
    restarts: 0,
};

const IMAGE_FS: ComponentSpec<'static> = ComponentSpec {
    executable: "/filesystem-imagefs",
    name: "filesystem-imagefs",
    ..RAM_FS
};

const CRASH_FS: ComponentSpec<'static> = ComponentSpec {
    executable: "/crash-filesystem",
    name: "crash-filesystem",
    restarts: 1,
    ..RAM_FS
};

const FEATURED: ComponentSpec<'static> =
    fixture("/featured-export-fixture", "featured-export-fixture");

pub(crate) const PUBLISHER: ComponentSpec<'static> =
    fixture("/feature-publisher-fixture", "feature-publisher-fixture");

const FEATURE_HOLDER: ComponentSpec<'static> = ComponentSpec {
    restarts: 1,
    ..fixture("/feature-holder-fixture", "feature-holder-fixture")
};

const FEATURED_QUERY: &[Attribute<'static>] = &[pair("kind", "featured")];

const SCOPE_CONSUMER: ComponentSpec<'static> =
    fixture("/scope-consumer-fixture", "scope-consumer-fixture");

pub(crate) const fn fixture(
    executable: &'static str,
    name: &'static str,
) -> ComponentSpec<'static> {
    ComponentSpec {
        executable,
        name,
        memory_limit: MIB,
        restarts: 0,
    }
}

pub(crate) const fn probe_of(name: &'static str) -> crate::composition::Import {
    crate::composition::Import {
        declaration: RequirementDeclaration::all(name, probe::Protocol::ID),

        namespace_root: None,
    }
}

pub(crate) fn scenario(components: &[ComponentSpec<'_>]) -> Composition {
    limited(components, SEARCH_LIMITS)
}

pub(crate) fn limited(components: &[ComponentSpec<'_>], limits: SearchLimits) -> Composition {
    started(
        &[&[PARENT], components].concat(),
        ExportIndex::EMPTY,
        limits,
    )
    .0
}

pub(crate) fn with_drivers() -> Composition {
    let composition = started(&[PARENT, PLATFORM], image_index(), SEARCH_LIMITS).0;
    // `platform` отвечает на сессии после публикации признаков, и запрос к драйверу их учитывает.
    let importer = composition.importer(&[platform_of(PLATFORM_REQUIREMENT)]);
    platform_session(&importer, PLATFORM_REQUIREMENT)
        .description()
        .expect("platform description");
    composition
}

pub(crate) fn indexed(components: &[ComponentSpec<'_>]) -> (Composition, ComponentNames) {
    started(
        &[&[PARENT], components].concat(),
        image_index(),
        SEARCH_LIMITS,
    )
}

fn image_index() -> ExportIndex<'static> {
    ExportIndex::new(image().entries().expect("image entries"), BOOT_PLAN_ENTRY)
}

fn started(
    records: &[ComponentSpec<'_>],
    index: ExportIndex<'static>,
    limits: SearchLimits,
) -> (Composition, ComponentNames) {
    let plan = encoded(records);
    let names = ComponentNames::of(&plan);
    start_component_relay(names.clone());
    let composition = Composition::start(
        plan,
        index,
        LogSources::new(names.clone(), ScenarioLogSessions),
        limits,
    );
    (composition, names)
}

pub(crate) fn registered(
    composition: &Composition,
    names: &ComponentNames,
    spec: ComponentSpec<'static>,
) -> Result<u16, rootkeeper::AddFailure> {
    let entry = image()
        .entries()
        .expect("image entries")
        .entry(&format!("{}/exec", spec.executable))
        .expect("fixture program");
    let program = userland::decode_program(entry.data()).expect("fixture program decodes");
    let badge = names.add(spec.name).expect("log source added");
    composition.try_add(
        rootkeeper::Component {
            name: ProcessName::new(spec.name).expect("fixture process name"),
            executable: spec.executable,
            program,
            memory_limit: spec.memory_limit,
            restarts: spec.restarts,
        },
        ScenarioLogSessions.session(badge).expect("log session"),
    )
}

pub(crate) fn finish(composition: Composition) {
    drop(composition);
    stop_component_relay();
}

fn ping(importer: &Importer, requirement: &str) -> Result<u64, IpcError> {
    importer
        .session::<probe::Protocol>(requirement)
        .client()
        .with_wait(COMPONENT_REPLY_TIMEOUT)
        .ping(PING)
}

fn ping_found(found: &Found) -> Result<u64, IpcError> {
    component_startup::open::<probe::Protocol>(found, COMPONENT_REPLY_TIMEOUT)
        .map_err(|_| IpcError::PeerClosed)?
        .client()
        .with_wait(COMPONENT_REPLY_TIMEOUT)
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
    for _ in 0..POLL_ATTEMPTS {
        if found.len() == count {
            break;
        }
        pause(POLL_INTERVAL);
        found = importer.found::<probe::Protocol>(requirement);
    }
    found
}

fn enumerated(importer: &Importer, requirement: &str) -> Session<startup_contract::Protocol> {
    importer
        .enumeration::<probe::Protocol>(requirement, Attributes::empty())
        .expect("enumeration granted")
}

pub(crate) const fn pair(key: &'static str, value: &'static str) -> Attribute<'static> {
    Attribute {
        key: key.as_bytes(),
        value: value.as_bytes(),
    }
}

pub(crate) fn query(pairs: &[Attribute<'_>]) -> Vec<u8> {
    let mut bytes = alloc::vec![0; attributes_len(pairs)];
    encode_attributes_into(&mut bytes, pairs);
    bytes
}

fn probes_found(importer: &Importer, pairs: &[Attribute<'_>]) -> Vec<Found> {
    let bytes = query(pairs);
    importer
        .found_with::<probe::Protocol>(PROBE, Attributes::decode(&bytes).expect("query decodes"))
}

fn filters_found(importer: &Importer, pairs: &[Attribute<'_>]) -> Vec<String> {
    probes_found(importer, pairs)
        .iter()
        .map(|found| format!("{}", Pairs(found.filter.as_attributes())))
        .collect()
}

fn patched(data: &[u8], patch: impl FnOnce(&mut [u8])) -> userland::DecodedProgram<'static> {
    let program = userland::decode_program(data).expect("program decodes");
    let manifest = userland::ProgramView::manifest(&program);
    let start = manifest.as_ptr().addr() - data.as_ptr().addr();
    let end = start + manifest.len();

    let mut bytes = data.to_vec();
    patch(&mut bytes[start..end]);
    userland::decode_program(Vec::leak(bytes)).expect("program boundaries remain valid")
}

pub(crate) fn requested<P: Protocol>(
    importer: &Importer,
    requirement: &str,
    wait: Timeout,
) -> Result<(), CallError<SessionError>> {
    request::<P>(importer.handle(), requirement, wait).map(drop)
}

const fn rtc_of(name: &'static str) -> crate::composition::Import {
    crate::composition::Import {
        declaration: RequirementDeclaration::all(name, rtc::Protocol::ID),
        namespace_root: None,
    }
}

fn rtc_session(
    importer: &Importer,
) -> Result<rtc::Client<SessionTransport>, CallError<SessionError>> {
    request::<rtc::Protocol>(importer.handle(), RTC_REQUIREMENT, RTC_REPLY_TIMEOUT)
        .map(|session| session.client().with_wait(RTC_REPLY_TIMEOUT))
}

fn binding_lines(driver: &str, count: usize) -> Vec<String> {
    await_component_lines(driver, count)
}

const fn featured_of(name: &'static str) -> crate::composition::Import {
    crate::composition::Import {
        declaration: RequirementDeclaration::session(name, probe::Protocol::ID),
        namespace_root: None,
    }
}

fn featured_session(
    importer: Handle<kind::Port>,
    wait: Timeout,
) -> Result<u64, CallError<SessionError>> {
    let bytes = query(FEATURED_QUERY);
    let session = session::<probe::Protocol>(
        importer,
        FEATURED_REQUIREMENT,
        Attributes::decode(&bytes).expect("query decodes"),
        wait,
    )?;

    Ok(session
        .client()
        .with_wait(COMPONENT_REPLY_TIMEOUT)
        .ping(PING)?)
}

fn await_featured(importer: &Importer, count: usize) -> usize {
    let mut found = probes_found(importer, FEATURED_QUERY).len();
    for _ in 0..POLL_ATTEMPTS {
        if found == count {
            break;
        }

        pause(POLL_INTERVAL);
        found = probes_found(importer, FEATURED_QUERY).len();
    }

    found
}

pub(crate) const fn platform_of(name: &'static str) -> crate::composition::Import {
    crate::composition::Import {
        declaration: RequirementDeclaration::all(name, platform::Protocol::ID),
        namespace_root: None,
    }
}

pub(crate) fn platform_session(
    importer: &Importer,
    requirement: &str,
) -> platform::Client<SessionTransport> {
    importer
        .session::<platform::Protocol>(requirement)
        .client()
        .with_wait(COMPONENT_REPLY_TIMEOUT)
}

fn line(
    session: &platform::Client<SessionTransport>,
    device: &str,
) -> Result<IrqLine, CallError<PlatformError>> {
    session.irq(device, 0).map(IrqLine::from)
}

fn window(
    session: &platform::Client<SessionTransport>,
    device: &str,
) -> Result<MmioWindow<MemoryRegion>, CallError<PlatformError>> {
    session.mmio(device, 0).map(|window| MmioWindow {
        region: MemoryRegion::from(window.region),
        offset: window.offset,
        size: window.size,
    })
}

fn description(
    session: &platform::Client<SessionTransport>,
) -> Result<(MemoryRegion, u64), CallError<PlatformError>> {
    session
        .description()
        .map(|description| (MemoryRegion::from(description.region), description.offset))
}

pub(crate) fn platform_describes(device: &str) -> bool {
    with_description(|description| description.find(device).is_some())
}

pub(crate) fn platform_has_compatible(compatible: &str) -> bool {
    with_description(|description| description.compatible_devices(compatible).next().is_some())
}

fn described_clock() -> Option<(&'static str, &'static str)> {
    with_description(|description| {
        description
            .devices()
            .filter_map(Result::ok)
            .find_map(|device| {
                CLOCK_DRIVERS
                    .into_iter()
                    .find(|(compatible, _)| device.is_compatible(compatible))
            })
    })
}

fn with_description<T>(inspect: impl FnOnce(&Description<'_>) -> T) -> T {
    let composition = scenario(&[PLATFORM]);
    let importer = composition.importer(&[platform_of(PLATFORM_REQUIREMENT)]);
    let session = platform_session(&importer, PLATFORM_REQUIREMENT);

    let inspected = {
        let (region, offset) = description(&session).expect("description granted");
        let mapping = region
            .map_full_read_only()
            .expect("description maps read-only");
        inspect(&Description::new(mapping.as_bytes(), offset).expect("description parses"))
    };

    drop(session);
    finish(composition);
    inspected
}

fn region_base(region: &MemoryRegion) -> u64 {
    region.inspect().expect("region inspects").base
}

pub(crate) fn await_revoked<T>(inspect: impl Fn() -> Result<T, Error>) -> bool {
    (0..POLL_ATTEMPTS).any(|_| {
        let revoked = inspect().err() == Some(Error::Syscall(SyscallError::Revoked));
        if !revoked {
            pause(POLL_INTERVAL);
        }
        revoked
    })
}

fn held_request(found: Found, wait: Timeout) -> JoinHandle {
    spawn_owned(
        move || match component_startup::open::<probe::Protocol>(&found, wait) {
            Ok(_) => GRANTED,
            Err(CallError::Remote(SessionError::Unavailable)) => UNAVAILABLE,
            Err(CallError::Ipc(IpcError::Timeout)) => TIMED_OUT,
            _ => OTHER,
        },
    )
    .expect("request thread starts")
}

fn watched(importer: &Importer, pairs: &[Attribute<'_>]) -> Session<startup_contract::Protocol> {
    let bytes = query(pairs);
    importer
        .watch::<probe::Protocol>(PROBE, Attributes::decode(&bytes).expect("query decodes"))
        .expect("watch granted")
}

fn held_next(watch: &Session<startup_contract::Protocol>) -> JoinHandle {
    let raw = watch.handle().as_raw();
    let thread = spawn_owned(move || {
        let answer = startup_contract::Client::new(PortTransport::new(raw))
            .with_wait(HELD_REQUEST_TIMEOUT)
            .next();
        match answer {
            Ok(Some(_)) => GRANTED,
            Err(CallError::Remote(SessionError::Refused)) => REFUSED,
            Err(CallError::Ipc(IpcError::Timeout)) => TIMED_OUT,
            _ => OTHER,
        }
    })
    .expect("next thread starts");
    pause(REQUEST_DELIVERY_PAUSE);
    thread
}

fn watch_next(watch: &Session<startup_contract::Protocol>, wait: Timeout) -> Option<Found> {
    match component_startup::next(watch, wait) {
        Ok(found) => found,
        Err(CallError::Ipc(IpcError::Timeout)) => None,
        Err(error) => panic!("watch next answers: {error:?}"),
    }
}

fn answered(request: JoinHandle) -> u32 {
    request
        .wait(HELD_REQUEST_TIMEOUT)
        .expect("request thread ends");
    let code = request.exit_code().expect("request thread exit code");
    request
        .join(Timeout::POLL)
        .expect("request thread is joined");
    code
}

fn budget_scenario() -> (Composition, Importer) {
    let composition = scenario(&[ROUTER, RAM_FS, HOLDER, NEIGHBOUR]);
    let importer = composition.importer(&[probe_of(HELD), probe_of(PROBE), view(TMP_VIEW, "/tmp")]);
    let held = await_component_lines(HOLDER.name, 2);
    test_harness::check_eq!(
        held.iter().take(2).collect::<Vec<_>>(),
        ["budget held", STOPPED_LINE]
    );
    (composition, importer)
}

fn resolve(
    view: &Port,
    text: &str,
    wait: Timeout,
) -> Result<Session<file_system::Protocol>, CallError<FileSystemError>> {
    namespace::Client::new(PortTransport::new(view))
        .with_wait(wait)
        .resolve(text)
        .map(Session::from_session)
}

fn resolved(view: &Port, text: &str) -> file_system::Client<SessionTransport> {
    connect(resolve(view, text, RESOLVE_TIMEOUT).expect("path is covered by a mount"))
}

fn write(client: &file_system::Client<SessionTransport>) {
    open(
        client,
        WRITTEN_PATH,
        AccessMode::ReadWrite,
        OpenFlags::CREATE,
    );
}

fn resolution(view: &Port, text: &str, wait: Timeout) -> Result<(), CallError<FileSystemError>> {
    resolve(view, text, wait).map(drop)
}

fn written(
    client: &file_system::Client<SessionTransport>,
) -> Result<NodeKind, CallError<FileSystemError>> {
    client
        .open(WRITTEN_PATH, AccessMode::Read, OpenFlags::NONE)
        .map(|opened| opened.kind)
}

fn logged(lines: &[String], expected: &str) -> usize {
    lines.iter().filter(|line| *line == expected).count()
}

fn refusals() -> Vec<String> {
    let composition = scenario(&[EXPORT, REFUSAL_PROBE]);
    let lines = await_component_lines(REFUSAL_PROBE.name, 5);
    finish(composition);
    lines
}

#[test_harness::test]
fn a_log_badge_names_its_record() {
    let plan = encoded(&[PARENT, EXPORT, IDLE_EXPORT]);
    let names = ComponentNames::of(&plan);

    let owners = [1, 2].map(|record| {
        names
            .name(log_badge(record).get())
            .expect("record badge is named")
    });
    let unnamed = names.name(log_badge(3).get());

    test_harness::check_eq!(
        owners.each_ref().map(LogBuffer::as_str),
        [EXPORT.name, IDLE_EXPORT.name]
    );
    test_harness::check!(unnamed.is_none());
}

#[test_harness::test]
fn compiled_plan_carries_the_boot_composition() {
    let plan = image()
        .plan(BOOT_PLAN_ENTRY)
        .expect("compiled plan decodes");

    let file_systems = [
        "filesystem-imagefs",
        "unresponsive-filesystem",
        "filesystem-ramfs",
    ]
    .map(|name| {
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
        let serves_by_kind = manifest.exports().any(|export| {
            export.protocol() == file_system::Protocol::ID
                && export.filters().any(|filter| filter.value(b"fs").is_some())
        });
        let mounts_nothing = manifest
            .requirements()
            .all(|requirement| requirement.protocol() != file_system_mount::Protocol::ID);
        (serves_by_kind, mounts_nothing)
    });

    for (serves_by_kind, mounts_nothing) in file_systems {
        test_harness::check!(serves_by_kind);
        test_harness::check!(mounts_nothing);
    }
}

#[test_harness::test]
fn a_program_entry_read_as_a_plan_names_the_boot_failure() {
    let failure = image().plan(EXPORT_ENTRY).err();

    test_harness::check_eq!(
        failure,
        Some(BootFailure::Plan(PlanDecodeError::InvalidMagic(
            USERLAND_PROGRAM_MAGIC
        )))
    );
    test_harness::check_eq!(failure.map(BootFailure::stopped), Some(Stopped::BadImage));
}

#[test_harness::test]
fn a_requirement_outside_the_record_of_the_caller_is_refused_at_once() {
    let lines = refusals();

    test_harness::check!(
        lines
            .iter()
            .any(|line| line == "outside Err(Remote(Refused))")
    );
}

#[test_harness::test]
fn a_session_requirement_of_another_protocol_is_refused_at_once() {
    let lines = refusals();

    test_harness::check!(
        lines
            .iter()
            .any(|line| line == "protocol Err(Remote(Refused))")
    );
}

#[test_harness::test]
fn an_accept_of_an_export_outside_the_record_is_refused_at_once() {
    let lines = refusals();

    test_harness::check!(
        lines
            .iter()
            .any(|line| line == "accept Err(Remote(Refused))")
    );
}

#[test_harness::test]
fn a_log_requirement_of_another_protocol_is_refused_at_once() {
    let lines = refusals();

    test_harness::check!(lines.iter().any(|line| line == "log Err(Remote(Refused))"));
}

#[test_harness::test]
fn a_session_of_another_protocol_exported_by_record_zero_is_refused() {
    let composition = started(
        &[ComponentSpec {
            executable: "/export-fixture",
            ..PARENT
        }],
        ExportIndex::EMPTY,
        SEARCH_LIMITS,
    )
    .0;
    let importer = composition.importer(&[probe_of(PROBE)]);

    let answer = requested::<probe::Protocol>(&importer, PROBE, COMPONENT_REPLY_TIMEOUT);

    test_harness::check_eq!(answer, Err(CallError::Remote(SessionError::Refused)));
    finish(composition);
}

#[test_harness::test]
fn an_importer_cannot_take_the_requests_of_a_child_export() {
    let composition = scenario(&[EXPORT]);
    let importer = composition.importer(&[probe_of(PROBE)]);
    let parent = startup_contract::Client::new(PortTransport::new(importer.handle()))
        .with_wait(PARENT_REFUSAL_TIMEOUT);

    let taken = accept::<probe::Protocol>(&parent).map(drop);
    let log = session::<log::Protocol>(
        importer.handle(),
        LOG_REQUIREMENT,
        Attributes::empty(),
        PARENT_REFUSAL_TIMEOUT,
    )
    .map(drop);
    let pinged = ping(&importer, PROBE);

    test_harness::check_eq!(taken, Err(CallError::Remote(AcceptError::Refused)));
    test_harness::check_eq!(log, Err(CallError::Remote(SessionError::Refused)));
    test_harness::check_eq!(pinged, Ok(PING));
    finish(composition);
}

#[test_harness::test]
fn an_importer_stays_waiting_while_its_provider_lives_without_accept() {
    let composition = scenario(&[IDLE_EXPORT]);
    let importer = composition.importer(&[probe_of(PROBE)]);
    let started = await_component_lines(IDLE_EXPORT.name, 1);

    let waited = requested::<probe::Protocol>(&importer, PROBE, IDLE_PROVIDER_REQUEST_TIMEOUT);

    test_harness::check_eq!(started, ["started"]);
    test_harness::check_eq!(waited, Err(CallError::Ipc(IpcError::Timeout)));
    finish(composition);
}

#[test_harness::test]
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

    test_harness::check!(sources.iter().any(|source| source == "first-probe"));
    test_harness::check!(sources.iter().any(|source| source == "second-probe"));
    test_harness::check!(!sources.iter().any(|source| source.starts_with("badge-")));
    finish(composition);
}

#[test_harness::test]
fn a_child_log_session_refuses_duplicate() {
    let composition = scenario(&[LOG_DUPLICATE]);

    let lines = await_component_lines(LOG_DUPLICATE.name, 2);

    test_harness::check_eq!(lines, DUPLICATE_REFUSED_LINES);
    finish(composition);
}

#[test_harness::test]
fn the_importer_of_a_provider_out_of_restarts_gets_unavailable() {
    let composition = scenario(&[IDLE_EXPORT, NEIGHBOUR]);
    let importer = composition.importer(&[probe_of(HELD), probe_of(PROBE)]);
    let started = await_component_lines(IDLE_EXPORT.name, 1);
    let held = held_request(
        first_found::<probe::Protocol>(&importer, HELD),
        HELD_REQUEST_TIMEOUT,
    );
    let again = first_found::<probe::Protocol>(&importer, HELD);
    pause(REQUEST_DELIVERY_PAUSE);

    composition.terminate(IDLE_EXPORT.name, TERMINATE_EXIT_CODE);
    let held_answer = answered(held);
    let again =
        component_startup::open::<probe::Protocol>(&again, COMPONENT_REPLY_TIMEOUT).map(drop);
    let pinged = ping(&importer, PROBE);

    test_harness::check_eq!(started, ["started"]);
    test_harness::check_eq!(held_answer, UNAVAILABLE);
    test_harness::check_eq!(again, Err(CallError::Remote(SessionError::Unavailable)));
    test_harness::check_eq!(pinged, Ok(PING));
    finish(composition);
}

#[test_harness::test]
fn a_provider_that_exits_with_code_zero_leaves_its_importers_unavailable() {
    let composition = scenario(&[ComponentSpec {
        restarts: 2,
        ..EXPORT
    }]);
    let importer = composition.importer(&[probe_of(PROBE)]);
    let started = await_component_lines(EXPORT.name, 1);
    let found = first_found::<probe::Protocol>(&importer, PROBE);

    composition.terminate(EXPORT.name, 0);
    let answer =
        component_startup::open::<probe::Protocol>(&found, COMPONENT_REPLY_TIMEOUT).map(drop);
    let again = requested::<probe::Protocol>(&importer, PROBE, COMPONENT_REPLY_TIMEOUT);
    let lines = await_component_lines(EXPORT.name, 2);

    test_harness::check_eq!(started, ["started"]);
    test_harness::check_eq!(answer, Err(CallError::Remote(SessionError::Unavailable)));
    test_harness::check_eq!(again, Err(CallError::Remote(SessionError::Unbound)));
    test_harness::check_eq!(lines, ["started"]);
    finish(composition);
}

#[test_harness::test]
fn a_failing_child_spends_each_restart_once() {
    let composition = scenario(&[ComponentSpec {
        restarts: 2,
        ..LOG_DUPLICATE
    }]);

    let failures = await_component_lines(LOG_DUPLICATE.name, 6);
    let after = await_component_lines(LOG_DUPLICATE.name, 7);

    test_harness::check_eq!(failures, DUPLICATE_REFUSED_LINES.repeat(3));
    test_harness::check_eq!(after.len(), 6);
    finish(composition);
}

#[test_harness::test]
fn a_child_taken_down_for_a_closed_startup_session_spends_a_restart() {
    let composition = scenario(&[ComponentSpec {
        restarts: 1,
        ..CLOSED
    }]);

    let restarted = await_component_lines(CLOSED.name, 2);
    let after = await_component_lines(CLOSED.name, 3);

    test_harness::check_eq!(restarted.len(), 2);
    test_harness::check_eq!(after.len(), 2);
    finish(composition);
}

#[test_harness::test]
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
    let held = held_request(
        first_found::<probe::Protocol>(&importer, HELD),
        REQUEST_ACROSS_EXIT_TIMEOUT,
    );
    pause(REQUEST_DELIVERY_PAUSE);

    composition.terminate(leaving.name, TERMINATE_EXIT_CODE);
    let restarted = await_component_lines(leaving.name, 2);
    let held_answer = answered(held);

    test_harness::check_eq!(idle_started, ["started"]);
    test_harness::check_eq!(leaving_started, ["started"]);
    test_harness::check_eq!(restarted, ["started", "started"]);
    test_harness::check_eq!(held_answer, TIMED_OUT);
    finish(composition);
}

#[test_harness::test]
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

    test_harness::check_eq!(started, ["started"]);
    test_harness::check_eq!(incarnations, [2, 3]);
    test_harness::check_eq!(lines, ["started"]);
    finish(composition);
}

#[test_harness::test]
fn a_component_out_of_restarts_does_not_stop_the_root() {
    let composition = scenario(&[
        ComponentSpec {
            restarts: 1,
            ..ROUTER
        },
        NEIGHBOUR,
    ]);
    let importer = composition.importer(&[view(ROOT_VIEW, ""), probe_of(PROBE)]);
    // Без файловых систем каждое воплощение маршрутизатора пишет отказ строки `/boot`.
    let started = logged(
        &await_component_lines_with_prefix(ROUTER_NAME, BOOT_REFUSAL, 1),
        BOOT_REFUSAL,
    );
    let router = first_found::<namespace::Protocol>(&importer, ROOT_VIEW);

    composition.terminate(ROUTER_NAME, TERMINATE_EXIT_CODE);
    let restarted = logged(
        &await_component_lines_with_prefix(ROUTER_NAME, BOOT_REFUSAL, 2),
        BOOT_REFUSAL,
    );
    composition.terminate(ROUTER_NAME, TERMINATE_EXIT_CODE);
    let refused =
        component_startup::open::<namespace::Protocol>(&router, COMPONENT_REPLY_TIMEOUT).map(drop);
    let pinged = ping(&importer, PROBE);

    test_harness::check_eq!(started, 1);
    test_harness::check_eq!(restarted, 2);
    test_harness::check_eq!(refused, Err(CallError::Remote(SessionError::Unavailable)));
    test_harness::check_eq!(pinged, Ok(PING));
    finish(composition);
}

#[test_harness::test]
fn a_child_is_served_while_another_waits_for_its_budget() {
    let (composition, importer) = budget_scenario();

    let found = importer.found::<probe::Protocol>(PROBE);
    let served = found.get(1).map(|found| {
        component_startup::open::<probe::Protocol>(found, SERVED_CHILD_REPLY_TIMEOUT).map(drop)
    });
    let removed = resolved(&importer.port::<namespace::Protocol>(TMP_VIEW), "/")
        .unlink(HELD_PATH, NodeKind::File);
    let pinged = ping_found(&first_found::<probe::Protocol>(&importer, HELD));

    test_harness::check_eq!(served, Some(Ok(())));
    test_harness::check!(removed.is_ok());
    test_harness::check_eq!(pinged, Ok(PING));
    finish(composition);
}

#[test_harness::test]
fn a_child_whose_budget_did_not_return_in_time_stays_removed() {
    let (composition, importer) = budget_scenario();

    let held = component_startup::open::<probe::Protocol>(
        &first_found::<probe::Protocol>(&importer, HELD),
        BUDGET_RETURN_TIMEOUT,
    )
    .map(drop);
    let pinged = ping(&importer, PROBE);

    test_harness::check_eq!(held, Err(CallError::Remote(SessionError::Unavailable)));
    test_harness::check_eq!(pinged, Ok(PING));
    finish(composition);
}

#[test_harness::test]
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

    test_harness::check_eq!(found.len(), 1);
    test_harness::check_eq!(found_pinged, Some(Ok(PING)));
    test_harness::check_eq!(pinged, Ok(PING));
    finish(composition);
}

#[test_harness::test]
fn a_clock_driver_serves_time_from_its_described_device() {
    let Some((compatible, driver)) = described_clock() else {
        test_harness::skip!("no clock is described");
    };

    let composition = with_drivers();
    let importer = composition.importer(&[rtc_of(RTC_REQUIREMENT)]);
    let clock = rtc_session(&importer).expect("rtc session");

    let now = clock.now().expect("rtc answers the time");
    let ticked = clock.tick().expect("rtc alarm fires");
    drop(clock);
    let renewed = rtc_session(&importer).expect("rtc session after the last one closed");
    let again = renewed.now();
    drop(renewed);
    let lines = binding_lines(driver, 2);

    test_harness::check!(ticked >= now);
    test_harness::check!(again.is_ok());
    test_harness::check_eq!(lines, [format!("{compatible} bound")]);
    finish(composition);
}

#[test_harness::test]
fn a_session_to_a_hidden_export_is_unbound_until_the_feature_is_published() {
    let (composition, names) = indexed(&[]);
    let importer = composition.importer(&[featured_of(FEATURED_REQUIREMENT)]);

    let hidden = featured_session(importer.handle(), COMPONENT_REPLY_TIMEOUT);
    registered(&composition, &names, PUBLISHER).expect("publisher added");
    let published = await_component_lines(PUBLISHER.name, 1);
    let opened = featured_session(importer.handle(), COMPONENT_REPLY_TIMEOUT);

    test_harness::check_eq!(hidden, Err(CallError::Remote(SessionError::Unbound)));
    test_harness::check_eq!(published, ["published Ok(()) Ok(())"]);
    test_harness::check_eq!(opened, Ok(PING));
    finish(composition);
}

#[test_harness::test]
fn a_record_waits_for_a_restarted_publisher_and_starts_once_it_is_removed() {
    let (composition, names) =
        started(&[PARENT, FEATURE_HOLDER], ExportIndex::EMPTY, SEARCH_LIMITS);
    let holding = await_component_lines(FEATURE_HOLDER.name, 1);

    registered(&composition, &names, EXPORT).expect("record added");
    let held = await_component_lines(EXPORT.name, 1);
    composition.terminate(FEATURE_HOLDER.name, TERMINATE_EXIT_CODE);
    let restarted = await_component_lines(FEATURE_HOLDER.name, 2);
    let held_after_restart = await_component_lines(EXPORT.name, 1);
    composition.terminate(FEATURE_HOLDER.name, TERMINATE_EXIT_CODE);
    let released = await_component_lines(EXPORT.name, 1);

    test_harness::check_eq!(holding, ["started"]);
    test_harness::check!(held.is_empty());
    test_harness::check_eq!(restarted, ["started", "started"]);
    test_harness::check!(held_after_restart.is_empty());
    test_harness::check_eq!(released, ["started"]);
    finish(composition);
}

#[test_harness::test]
fn an_enumeration_finds_a_featured_export_only_while_its_publisher_lives() {
    let (composition, names) = indexed(&[]);
    let importer = composition.importer(&[probe_of(PROBE)]);

    let hidden = probes_found(&importer, FEATURED_QUERY).len();
    registered(&composition, &names, PUBLISHER).expect("publisher added");
    let published = await_featured(&importer, 1);
    composition.terminate(PUBLISHER.name, TERMINATE_EXIT_CODE);
    let left = await_featured(&importer, 0);

    test_harness::check_eq!(hidden, 0);
    test_harness::check_eq!(published, 1);
    test_harness::check_eq!(left, 0);
    finish(composition);
}

#[test_harness::test]
fn registration_refuses_a_plan_record_with_a_feature() {
    let (composition, names) = started(&[PARENT], ExportIndex::EMPTY, SEARCH_LIMITS);

    let added = registered(&composition, &names, FEATURED);

    test_harness::check!(matches!(added, Err(rootkeeper::AddFailure::Declaration)));
    finish(composition);
}

#[test_harness::test]
fn a_record_of_the_export_index_gets_the_root_exports_except_features() {
    let (composition, names) = indexed(&[]);
    let importer = composition.importer(&[featured_of(FEATURED_REQUIREMENT)]);
    registered(&composition, &names, PUBLISHER).expect("publisher added");
    await_component_lines(PUBLISHER.name, 1);

    let opened = featured_session(importer.handle(), COMPONENT_REPLY_TIMEOUT);
    let lines = await_component_lines(FEATURED.name, 6);

    test_harness::check_eq!(opened, Ok(PING));
    test_harness::check_eq!(
        lines,
        [
            "hardware Ok(())",
            "system_info Ok(true)",
            "image Ok(())",
            "features Err(Refused)",
            "started",
            "request kind=featured"
        ]
    );
    finish(composition);
}

#[test_harness::test]
fn a_restarted_child_takes_the_same_line_and_region_again() {
    if !platform_describes(PL031) {
        test_harness::skip!("{} is not described", PL031);
    }

    let composition = scenario(&[PLATFORM, IRQ]);

    let lines = await_component_lines(IRQ.name, 4);

    test_harness::check_eq!(lines.len(), 4);
    test_harness::check!(lines[0].starts_with("line and region "));
    test_harness::check_eq!(lines[1], STOPPED_LINE);
    test_harness::check_eq!(lines[2..], lines[..2]);
    finish(composition);
}

#[test_harness::test]
fn a_device_outside_the_platform_description_is_not_described() {
    let composition = scenario(&[PLATFORM]);
    let importer = composition.importer(&[platform_of(PLATFORM_REQUIREMENT)]);
    let session = platform_session(&importer, PLATFORM_REQUIREMENT);

    let absent_line = line(&session, ABSENT).map(drop);
    let absent_window = window(&session, ABSENT).map(drop);
    let relative_line = line(&session, RELATIVE).map(drop);

    test_harness::check_eq!(
        absent_line,
        Err(CallError::Remote(PlatformError::NotDescribed))
    );
    test_harness::check_eq!(
        absent_window,
        Err(CallError::Remote(PlatformError::NotDescribed))
    );
    test_harness::check_eq!(
        relative_line,
        Err(CallError::Remote(PlatformError::NotDescribed))
    );
    drop(session);
    finish(composition);
}

#[test_harness::test]
fn a_line_and_a_region_taken_by_a_session_are_granted_to_another_after_it_closes() {
    if !platform_describes(PL031) {
        test_harness::skip!("{} is not described", PL031);
    }

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

    test_harness::check_eq!(contested_line, Err(CallError::Remote(PlatformError::Taken)));
    test_harness::check_eq!(
        contested_window,
        Err(CallError::Remote(PlatformError::Taken))
    );
    test_harness::check!(region_revoked);
    test_harness::check!(line_revoked);
    test_harness::check_eq!(granted_line, Ok(()));
    test_harness::check_eq!(granted_window, Ok(()));
    drop(second);
    finish(composition);
}

#[test_harness::test]
fn a_window_in_the_region_of_another_session_is_taken() {
    if !platform_describes(FIRST_VIRTIO) {
        test_harness::skip!("{} is not described", FIRST_VIRTIO);
    }

    let composition = scenario(&[PLATFORM]);
    let importer = composition.importer(&[platform_of(PLATFORM_REQUIREMENT)]);
    let first = platform_session(&importer, PLATFORM_REQUIREMENT);
    let second = platform_session(&importer, PLATFORM_REQUIREMENT);
    let taken = window(&first, FIRST_VIRTIO).expect("first session takes the region");

    let contested = window(&second, SECOND_VIRTIO).map(drop);

    test_harness::check_eq!(contested, Err(CallError::Remote(PlatformError::Taken)));
    drop((first, second, taken));
    finish(composition);
}

#[test_harness::test]
fn windows_of_one_session_in_one_page_share_the_region_at_their_offsets_and_sizes() {
    if !platform_describes(FIRST_VIRTIO) {
        test_harness::skip!("{} is not described", FIRST_VIRTIO);
    }

    let composition = scenario(&[PLATFORM]);
    let importer = composition.importer(&[platform_of(PLATFORM_REQUIREMENT)]);
    let session = platform_session(&importer, PLATFORM_REQUIREMENT);

    let first = window(&session, FIRST_VIRTIO).expect("first window");
    let second = window(&session, SECOND_VIRTIO).expect("second window");

    test_harness::check_eq!(region_base(&first.region), VIRTIO_REGION);
    test_harness::check_eq!(region_base(&second.region), VIRTIO_REGION);
    test_harness::check_eq!((first.offset, second.offset), (0, VIRTIO_STRIDE));
    test_harness::check_eq!((first.size, second.size), (VIRTIO_STRIDE, VIRTIO_STRIDE));
    test_harness::check!(
        first.region.inspect().expect("region inspects").size_bytes > VIRTIO_STRIDE
    );
    drop(session);
    finish(composition);
}

#[test_harness::test]
fn a_write_combining_mapping_of_a_window_starts_at_its_device() {
    if !platform_describes(SECOND_VIRTIO) {
        test_harness::skip!("{} is not described", SECOND_VIRTIO);
    }

    let composition = scenario(&[PLATFORM]);
    let importer = composition.importer(&[platform_of(PLATFORM_REQUIREMENT)]);
    let session = platform_session(&importer, PLATFORM_REQUIREMENT);
    let granted = window(&session, SECOND_VIRTIO).expect("second window");
    let device = kernel_objects::MmioWindow {
        region: granted.region,
        offset: granted.offset,
        size: granted.size,
    };

    let mapping = device
        .map_write_combining()
        .expect("device window maps write-combining");
    let magic: u32 = mapping.mmio().read(0);
    let device_end = usize::try_from(device.size).expect("window size fits usize");
    let past_end = mapping.copy_in_at(device_end - 1, &[0u8; 2]);

    test_harness::check_eq!(magic, VIRTIO_MAGIC);
    test_harness::check_eq!(past_end, Err(Error::OutOfBounds));
    drop(mapping);
    drop(session);
    finish(composition);
}

#[test_harness::test]
fn the_grants_of_a_platform_incarnation_are_revoked_when_it_ends() {
    if !platform_describes(PL031) {
        test_harness::skip!("{} is not described", PL031);
    }

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

    test_harness::check!(region_revoked);
    test_harness::check!(line_revoked);
    test_harness::check_eq!(renewed_line, Ok(()));
    test_harness::check_eq!(renewed_window, Ok(()));
    drop((session, renewed));
    finish(composition);
}

#[test_harness::test]
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

    test_harness::check_eq!(
        (region_base(&repeated), repeated_offset),
        (region_base(&region), offset)
    );
    test_harness::check_eq!(
        (region_base(&other), other_offset),
        (region_base(&region), offset)
    );
    test_harness::check_eq!(writable, Err(Error::Syscall(SyscallError::AccessDenied)));
    test_harness::check!(tree.devices().next().is_some());
    drop((first, second));
    finish(composition);
}

#[test_harness::test]
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

    test_harness::check!(region_revoked);
    test_harness::check!(copy_revoked);
    test_harness::check_eq!(renewed, Ok(()));
    drop(second);
    finish(composition);
}

#[test_harness::test]
fn the_image_region_of_an_image_file_system_incarnation_is_revoked_when_it_ends() {
    let composition = scenario(&[ROUTER, IMAGE_FS]);
    let importer = composition.importer(&[view(ROOT_VIEW, "")]);
    let files = resolved(
        &importer.port::<namespace::Protocol>(ROOT_VIEW),
        IMAGE_MOUNT,
    );
    let opened = open(&files, IMAGE_FILE, AccessMode::Read, OpenFlags::NONE);
    let window = MemoryRegion::from(files.backing_memory(opened.fd).expect("window granted"));
    let granted = window.inspect().is_ok();

    composition.terminate(IMAGE_FS.name, TERMINATE_EXIT_CODE);
    let revoked = await_revoked(|| window.inspect());

    test_harness::check!(granted);
    test_harness::check!(revoked);
    drop(files);
    finish(composition);
}

#[test_harness::test]
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
    let directory =
        resolve(&root, AREA_SCOPE, RESOLVE_TIMEOUT).expect("component directory exists");
    let mounted = Mounted::new(&importer, AREA, directory);

    let lines = await_component_lines(SCOPE_CONSUMER.name, 4);

    test_harness::check_eq!(lines, ["area read", STOPPED_LINE].repeat(2));
    drop(mounted);
    finish(composition);
}

#[test_harness::test]
fn a_component_that_faults_is_restarted() {
    let composition = scenario(&[ROUTER, CRASH_FS]);
    let importer = composition.importer(&[view(FILES, "/crash")]);
    let filesystem = importer.port::<namespace::Protocol>(FILES);
    let faulting = resolved(&filesystem, "/");
    write(&faulting);

    let renamed = faulting.rename(WRITTEN_PATH, RENAMED_PATH);
    let restarted = resolved(&filesystem, "/");
    let rewritten = written(&restarted);

    test_harness::check!(matches!(renamed, Err(CallError::Ipc(_))));
    test_harness::check_eq!(rewritten, Err(CallError::Remote(FileSystemError::NotFound)));
    finish(composition);
}

#[test_harness::test]
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
    // После конца монтирования разрешение ждёт монтирования новым воплощением.
    await_component_lines_with_prefix(ROUTER_NAME, TMP_ENDED, 1);
    let rewritten = written(&resolved(&tmp, "/"));
    let router = await_component_lines_with_prefix(ROUTER_NAME, TMP_MOUNT, 3);

    test_harness::check_eq!(logged(&router, TMP_MOUNT), 2);
    test_harness::check_eq!(rewritten, Err(CallError::Remote(FileSystemError::NotFound)));
    finish(composition);
}

#[test_harness::test]
fn a_restarted_router_mounts_its_table_again() {
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
    let mounted = resolution(&before, IMAGE_MOUNT, RESOLVE_TIMEOUT);

    composition.terminate(ROUTER_NAME, TERMINATE_EXIT_CODE);
    let renewed = [(ROOT_VIEW, IMAGE_MOUNT), (TMP_VIEW, "/")].map(|(requirement, text)| {
        let port = importer.port::<namespace::Protocol>(requirement);
        resolution(&port, text, RESOLVE_TIMEOUT)
    });
    let stale = resolution(&before, IMAGE_MOUNT, STALE_VIEW_TIMEOUT);

    test_harness::check_eq!(mounted, Ok(()));
    test_harness::check_eq!(renewed, [Ok(()), Ok(())]);
    test_harness::check!(matches!(stale, Err(CallError::Ipc(_))));
    finish(composition);
}

#[test_harness::test]
fn router_restart_keeps_file_system_components_and_their_sessions() {
    let composition = scenario(&[
        ComponentSpec {
            restarts: 1,
            ..ROUTER
        },
        RAM_FS,
    ]);
    let importer = composition.importer(&[view(TMP_VIEW, "/tmp"), view(SPARE_VIEW, "/spare")]);
    let files = resolved(&importer.port::<namespace::Protocol>(TMP_VIEW), "/");
    write(&files);
    write(&resolved(
        &importer.port::<namespace::Protocol>(SPARE_VIEW),
        "/",
    ));

    composition.terminate(ROUTER_NAME, TERMINATE_EXIT_CODE);
    let tmp = resolved(&importer.port::<namespace::Protocol>(TMP_VIEW), "/");
    let renewed = written(&tmp);
    let kept = written(&files);
    let volume = written(&resolved(
        &importer.port::<namespace::Protocol>(SPARE_VIEW),
        "/",
    ));

    test_harness::check_eq!(renewed, Ok(NodeKind::File));
    test_harness::check_eq!(kept, Ok(NodeKind::File));
    test_harness::check_eq!(volume, Ok(NodeKind::File));
    finish(composition);
}

#[test_harness::test]
fn malformed_and_unserved_table_lines_are_logged_while_the_rest_mount() {
    let composition = scenario(&[ROUTER, RAM_FS]);
    let importer = composition.importer(&[view(TMP_VIEW, "/tmp"), view(SPARE_VIEW, "/spare")]);
    let tmp = resolution(
        &importer.port::<namespace::Protocol>(TMP_VIEW),
        "/",
        RESOLVE_TIMEOUT,
    );
    let spare = resolution(
        &importer.port::<namespace::Protocol>(SPARE_VIEW),
        "/",
        RESOLVE_TIMEOUT,
    );
    // Три отказа разбора, монтирования `/tmp` и `/spare` и отказы строк `/boot`, `/stuck` и
    // `/crash`. Строка `/boot/spare` ждёт монтирования `/boot`.
    let router = await_component_lines(ROUTER_NAME, 8);
    let logged = |prefix: &str| router.iter().any(|line| line.starts_with(prefix));

    test_harness::check_eq!(tmp, Ok(()));
    test_harness::check_eq!(spare, Ok(()));
    test_harness::check!(logged("mount table line 12: InvalidPath"));
    test_harness::check!(logged("mount table line 13: MissingAttributes"));
    test_harness::check!(logged("mount table line 14: InvalidAttribute"));
    test_harness::check!(logged("mount table line 2 /boot: filesystem refused"));
    test_harness::check!(logged("mount table line 9 /crash: filesystem refused"));
    finish(composition);
}

#[test_harness::test]
fn a_removed_file_system_frees_its_prefix() {
    let composition = scenario(&[ROUTER, IMAGE_FS, RAM_FS]);
    let importer =
        composition.importer(&[view(ROOT_VIEW, ""), view(SPARE_VIEW, NESTED_SPARE_MOUNT)]);
    let spare = importer.port::<namespace::Protocol>(SPARE_VIEW);
    let mounted = resolution(&spare, "/", RESOLVE_TIMEOUT);

    composition.terminate(RAM_FS.name, TERMINATE_EXIT_CODE);
    let freed = resolution(&spare, "/", FREED_PREFIX_TIMEOUT);
    let root = importer.port::<namespace::Protocol>(ROOT_VIEW);
    let nested = resolution(&root, NESTED_SPARE_PATH, RESOLVE_TIMEOUT);
    // После снятия `/boot/spare` строка получает отказ `Unbound` либо `Unavailable` в
    // зависимости от того, снята ли запись к повторному запросу строки.
    let refusal = "mount table line 5 /boot/spare: filesystem refused";
    let router = await_component_lines_with_prefix(ROUTER_NAME, refusal, 1);
    let refused = router.iter().any(|line| line.starts_with(refusal));

    test_harness::check_eq!(mounted, Ok(()));
    test_harness::check_eq!(freed, Err(CallError::Ipc(IpcError::Timeout)));
    test_harness::check_eq!(nested, Err(CallError::Remote(FileSystemError::NotFound)));
    test_harness::check!(refused);
    finish(composition);
}

#[test_harness::test]
fn a_nested_table_line_mounts_after_a_mount_of_its_parent_path() {
    let composition = scenario(&[ROUTER, RAM_FS]);
    let importer = composition.importer(&[
        view(BOOT_VIEW, IMAGE_MOUNT),
        admin("boot-admin"),
        view(SPARE_VIEW, "/spare"),
        view(NESTED_VIEW, NESTED_SPARE_MOUNT),
    ]);
    let spare = resolve(
        &importer.port::<namespace::Protocol>(SPARE_VIEW),
        "/",
        RESOLVE_TIMEOUT,
    )
    .expect("spare mounted");
    let nested = importer.port::<namespace::Protocol>(NESTED_VIEW);
    let before = resolution(&nested, "/", FREED_PREFIX_TIMEOUT);

    let mounted = Mounted::new(&importer, BOOT_VIEW, Session::from_session(spare));
    let after = resolution(&nested, "/", RESOLVE_TIMEOUT);
    let parent = resolution(
        &importer.port::<namespace::Protocol>(BOOT_VIEW),
        "/",
        RESOLVE_TIMEOUT,
    );

    test_harness::check_eq!(before, Err(CallError::Ipc(IpcError::Timeout)));
    test_harness::check_eq!(after, Ok(()));
    test_harness::check_eq!(parent, Ok(()));
    drop(mounted);
    finish(composition);
}

#[test_harness::test]
fn a_mount_ends_when_its_file_system_closes_its_port() {
    let composition = scenario(&[ROUTER, RAM_FS]);
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
                RESOLVE_TIMEOUT,
            )
            .expect("spare mounted"),
        ),
    );
    let other = importer.port::<namespace::Protocol>(OTHER_VIEW);

    composition.terminate(RAM_FS.name, TERMINATE_EXIT_CODE);
    let freed = resolution(&other, "/", FREED_PREFIX_TIMEOUT);
    let ended = mounted.ended(COMPONENT_REPLY_TIMEOUT);

    test_harness::check_eq!(freed, Err(CallError::Ipc(IpcError::Timeout)));
    test_harness::check!(ended);
    drop(mounted);
    finish(composition);
}

#[test_harness::test]
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

    test_harness::check_eq!(
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

#[test_harness::test]
fn a_started_consumer_discovers_a_provider_added_at_runtime() {
    let consumer = ComponentSpec {
        ..fixture("/multiplicity-fixture", "multiplicity")
    };
    let (composition, names) = started(&[PARENT, consumer], ExportIndex::EMPTY, SEARCH_LIMITS);
    let started = await_component_lines(consumer.name, 5);

    registered(&composition, &names, EXPORT).expect("runtime component added");
    let lines = await_component_lines(consumer.name, 6);

    test_harness::check_eq!(
        started,
        [
            "optional empty",
            "started",
            "providers 0",
            "all refused",
            "image found"
        ]
    );
    test_harness::check_eq!(
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

#[test_harness::test]
fn registration_refuses_invalid_declarations_without_stopping_the_parent() {
    let plan = encoded(&[PARENT, EXPORT]);
    let names = ComponentNames::of(&plan);
    start_component_relay(names.clone());
    let composition = Composition::start(
        plan,
        ExportIndex::EMPTY,
        LogSources::new(names.clone(), ScenarioLogSessions),
        SEARCH_LIMITS,
    );
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
        executable: EXPORT.executable,
        program,
        memory_limit: EXPORT.memory_limit,
        restarts: 0,
    };
    let damaged = patched(entry.data(), |manifest| manifest[0] ^= 1);

    let failure = composition.try_add(
        component(damaged),
        ScenarioLogSessions.session(badge).expect("log"),
    );
    let pinged = ping(&importer, PROBE);
    let before = enumerated(&importer, PROBE);
    let held = first_found::<probe::Protocol>(&importer, PROBE);
    let record = composition
        .try_add(
            component(program),
            ScenarioLogSessions.session(badge).expect("log"),
        )
        .expect("same name can register after refusal");
    let found = importer.found::<probe::Protocol>(PROBE);
    let found_pinged = found.get(1).map(ping_found);
    let drained_count = drained(&before).len();
    let held_pinged = ping_found(&held);

    test_harness::check!(matches!(failure, Err(rootkeeper::AddFailure::Declaration)));
    test_harness::check_eq!(pinged, Ok(PING));
    test_harness::check_eq!(record, 2);
    test_harness::check_eq!(found.len(), 2);
    test_harness::check_eq!(found_pinged, Some(Ok(PING)));
    test_harness::check_eq!(drained_count, 1);
    test_harness::check_eq!(held_pinged, Ok(PING));
    finish(composition);
}

#[test_harness::test]
fn two_volumes_of_one_file_system_keep_their_trees_separate() {
    let composition = scenario(&[ROUTER, RAM_FS]);
    let importer = composition.importer(&[view(TMP_VIEW, "/tmp"), view(SPARE_VIEW, "/spare")]);
    let first = resolved(&importer.port::<namespace::Protocol>(TMP_VIEW), "/");
    let second = resolved(&importer.port::<namespace::Protocol>(SPARE_VIEW), "/");

    write(&first);
    let first_written = written(&first);
    let second_written = written(&second);

    test_harness::check_eq!(first_written, Ok(NodeKind::File));
    test_harness::check_eq!(
        second_written,
        Err(CallError::Remote(FileSystemError::NotFound))
    );
    finish(composition);
}

#[test_harness::test]
fn a_requirement_without_providers_enumerates_nothing() {
    let composition = scenario(&[]);
    let importer = composition.importer(&[probe_of(PROBE)]);

    let found = importer.found::<probe::Protocol>(PROBE);

    test_harness::check!(found.is_empty());
    finish(composition);
}

#[test_harness::test]
fn an_enumeration_and_a_watch_by_a_session_requirement_are_refused() {
    let composition = scenario(&[EXPORT]);
    let importer = composition.importer(&[crate::composition::Import {
        declaration: RequirementDeclaration::session(PROBE, probe::Protocol::ID),
        namespace_root: None,
    }]);

    let answer = importer
        .enumeration::<probe::Protocol>(PROBE, Attributes::empty())
        .map(drop);
    let watch = importer
        .watch::<probe::Protocol>(PROBE, Attributes::empty())
        .map(drop);
    let pinged = ping(&importer, PROBE);

    test_harness::check_eq!(answer, Err(CallError::Remote(SessionError::Refused)));
    test_harness::check_eq!(watch, Err(CallError::Remote(SessionError::Refused)));
    test_harness::check_eq!(pinged, Ok(PING));
    finish(composition);
}

#[test_harness::test]
fn a_watch_delivers_an_export_whose_feature_is_published_later() {
    let (composition, names) = indexed(&[]);
    let importer = composition.importer(&[probe_of(PROBE)]);
    let watch = watched(&importer, FEATURED_QUERY);

    let pending = held_next(&watch);
    registered(&composition, &names, PUBLISHER).expect("publisher added");

    test_harness::check_eq!(answered(pending), GRANTED);
    finish(composition);
}

#[test_harness::test]
fn a_watch_delivers_each_visible_export_and_then_waits() {
    let composition = scenario(&[EXPORT, NEIGHBOUR]);
    let importer = composition.importer(&[probe_of(PROBE)]);
    let watch = watched(&importer, &[]);

    let first = watch_next(&watch, COMPONENT_REPLY_TIMEOUT).expect("first export found");
    let second = watch_next(&watch, COMPONENT_REPLY_TIMEOUT).expect("second export found");
    let third = watch_next(&watch, WATCH_WAIT_TIMEOUT);

    test_harness::check_eq!(ping_found(&first), Ok(PING));
    test_harness::check_eq!(ping_found(&second), Ok(PING));
    test_harness::check!(third.is_none());
    finish(composition);
}

#[test_harness::test]
fn a_watch_does_not_deliver_an_image_program_again_after_its_record_starts() {
    let (composition, names) = indexed(&[]);
    let importer = composition.importer(&[probe_of(PROBE)]);
    registered(&composition, &names, PUBLISHER).expect("publisher added");
    await_component_lines(PUBLISHER.name, 1);
    let watch = watched(&importer, FEATURED_QUERY);

    let found = watch_next(&watch, COMPONENT_REPLY_TIMEOUT).expect("featured export found");
    let pinged = ping_found(&found);
    let again = watch_next(&watch, WATCH_WAIT_TIMEOUT);

    test_harness::check_eq!(pinged, Ok(PING));
    test_harness::check!(again.is_none());
    finish(composition);
}

#[test_harness::test]
fn a_second_next_of_a_watch_refuses_the_waiting_one() {
    let composition = scenario(&[]);
    let importer = composition.importer(&[probe_of(PROBE)]);
    let watch = watched(&importer, &[]);

    let pending = held_next(&watch);
    let second = component_startup::next(&watch, WATCH_WAIT_TIMEOUT).map(|found| found.is_some());

    test_harness::check_eq!(answered(pending), REFUSED);
    test_harness::check_eq!(second, Err(CallError::Ipc(IpcError::Timeout)));
    finish(composition);
}

#[test_harness::test]
fn a_watch_next_that_timed_out_leaves_the_export_for_the_next_call() {
    let (composition, names) = indexed(&[]);
    let importer = composition.importer(&[probe_of(PROBE)]);
    let watch = watched(&importer, FEATURED_QUERY);

    let hidden = watch_next(&watch, WATCH_WAIT_TIMEOUT);
    registered(&composition, &names, PUBLISHER).expect("publisher added");
    await_component_lines(PUBLISHER.name, 1);
    let found = watch_next(&watch, COMPONENT_REPLY_TIMEOUT);

    test_harness::check!(hidden.is_none());
    test_harness::check_eq!(found.as_ref().map(ping_found), Some(Ok(PING)));
    finish(composition);
}

#[test_harness::test]
fn a_new_watch_delivers_the_exports_of_a_closed_one_again() {
    let composition = scenario(&[EXPORT]);
    let importer = composition.importer(&[probe_of(PROBE)]);
    let closed = watched(&importer, &[]);
    watch_next(&closed, COMPONENT_REPLY_TIMEOUT).expect("export found");
    let waiting = watch_next(&closed, WATCH_WAIT_TIMEOUT);

    drop(closed);
    let watch = watched(&importer, &[]);
    let found = watch_next(&watch, COMPONENT_REPLY_TIMEOUT);

    test_harness::check!(waiting.is_none());
    test_harness::check_eq!(found.as_ref().map(ping_found), Some(Ok(PING)));
    finish(composition);
}

#[test_harness::test]
fn a_watch_delivers_a_restarted_provider_again() {
    let composition = scenario(&[ComponentSpec {
        restarts: 1,
        ..EXPORT
    }]);
    let importer = composition.importer(&[probe_of(PROBE)]);
    let watch = watched(&importer, &[]);
    watch_next(&watch, COMPONENT_REPLY_TIMEOUT).expect("export found");
    let waiting = watch_next(&watch, WATCH_WAIT_TIMEOUT);

    composition.terminate(EXPORT.name, TERMINATE_EXIT_CODE);
    let restarted = watch_next(&watch, HELD_REQUEST_TIMEOUT);
    await_component_lines(EXPORT.name, 2);

    test_harness::check!(waiting.is_none());
    test_harness::check_eq!(restarted.as_ref().map(ping_found), Some(Ok(PING)));
    finish(composition);
}

#[test_harness::test]
fn a_session_with_two_matching_exports_is_refused() {
    let composition = scenario(&[EXPORT, NEIGHBOUR]);
    let importer = composition.importer(&[probe_of(PROBE)]);

    let answer = requested::<probe::Protocol>(&importer, PROBE, COMPONENT_REPLY_TIMEOUT);
    let found_count = importer.found::<probe::Protocol>(PROBE).len();

    test_harness::check_eq!(answer, Err(CallError::Remote(SessionError::Refused)));
    test_harness::check_eq!(found_count, 2);
    finish(composition);
}

#[test_harness::test]
fn a_session_matching_a_record_and_an_image_program_is_refused() {
    let (composition, _) = indexed(&[EXPORT]);
    let importer = composition.importer(&[probe_of(PROBE)]);

    let answer = requested::<probe::Protocol>(&importer, PROBE, COMPONENT_REPLY_TIMEOUT);
    let found_count = importer.found::<probe::Protocol>(PROBE).len();

    test_harness::check_eq!(answer, Err(CallError::Remote(SessionError::Refused)));
    test_harness::check!(found_count > 1);
    finish(composition);
}

#[test_harness::test]
fn a_found_export_opens_only_as_its_protocol() {
    let composition = scenario(&[EXPORT]);
    let importer = composition.importer(&[probe_of(PROBE)]);
    let found = first_found::<probe::Protocol>(&importer, PROBE);

    let other = component_startup::open::<rtc::Protocol>(&found, STARTUP_REQUEST_TIMEOUT).map(drop);
    let all = importer
        .enumeration::<rtc::Protocol>(PROBE, Attributes::empty())
        .map(drop);
    let pinged = ping_found(&found);

    test_harness::check_eq!(other, Err(CallError::Remote(SessionError::Refused)));
    test_harness::check_eq!(all, Err(CallError::Remote(SessionError::Refused)));
    test_harness::check_eq!(pinged, Ok(PING));
    finish(composition);
}

#[test_harness::test]
fn next_skips_an_export_removed_after_all() {
    let composition = scenario(&[EXPORT, NEIGHBOUR]);
    let importer = composition.importer(&[probe_of(PROBE)]);
    let started = await_component_lines(EXPORT.name, 1);
    let enumeration = enumerated(&importer, PROBE);

    composition.terminate(EXPORT.name, 0);
    let remaining = await_found(&importer, PROBE, 1).len();
    let rest = drained(&enumeration);
    let rest_pinged = rest.first().map(ping_found);

    test_harness::check_eq!(started, ["started"]);
    test_harness::check_eq!(remaining, 1);
    test_harness::check_eq!(rest.len(), 1);
    test_harness::check_eq!(rest_pinged, Some(Ok(PING)));
    finish(composition);
}

#[test_harness::test]
fn a_filtered_export_answers_with_the_first_matching_filter() {
    let composition = scenario(&[EXPORT, FILTERED]);
    let importer = composition.importer(&[probe_of(PROBE)]);

    let unfiltered = filters_found(&importer, &[]);
    let first_matching = filters_found(&importer, &[pair("kind", "a"), pair("role", "c")]);
    let unmatched = filters_found(&importer, &[pair("kind", "b")]);
    let with_extra = filters_found(
        &importer,
        &[pair("kind", "b"), pair("role", "c"), pair("extra", "d")],
    );
    let role_only = filters_found(&importer, &[pair("role", "c")]);
    let bytes = query(&[pair("role", "c")]);
    let filtered = session::<probe::Protocol>(
        importer.handle(),
        PROBE,
        Attributes::decode(&bytes).expect("query decodes"),
        COMPONENT_REPLY_TIMEOUT,
    )
    .expect("one export matches")
    .client()
    .with_wait(COMPONENT_REPLY_TIMEOUT)
    .ping(PING);

    test_harness::check_eq!(unfiltered, [""]);
    test_harness::check_eq!(first_matching, ["kind=a"]);
    test_harness::check!(unmatched.is_empty());
    test_harness::check_eq!(with_extra, ["kind=b,role=c"]);
    test_harness::check_eq!(role_only, ["role=c"]);
    test_harness::check_eq!(filtered, Ok(PING));
    finish(composition);
}

#[test_harness::test]
fn a_session_request_carries_pairs_outside_the_matched_filter() {
    let composition = scenario(&[FILTERED]);
    let importer = composition.importer(&[probe_of(PROBE)]);
    let bytes = query(&[pair("kind", "a"), pair("extra", "x")]);

    let pinged = session::<probe::Protocol>(
        importer.handle(),
        PROBE,
        Attributes::decode(&bytes).expect("query decodes"),
        COMPONENT_REPLY_TIMEOUT,
    )
    .expect("the filter kind=a matches")
    .client()
    .with_wait(COMPONENT_REPLY_TIMEOUT)
    .ping(PING);
    let lines = await_component_lines(FILTERED.name, 2);

    test_harness::check_eq!(pinged, Ok(PING));
    test_harness::check_eq!(lines, ["started", "request kind=a,extra=x"]);
    finish(composition);
}

#[test_harness::test]
fn an_opened_search_result_carries_the_attributes_of_its_search() {
    let composition = scenario(&[FILTERED]);
    let importer = composition.importer(&[probe_of(PROBE)]);

    let found = probes_found(&importer, &[pair("role", "c")]);
    let pinged = found.first().map(ping_found);
    let lines = await_component_lines(FILTERED.name, 2);

    test_harness::check_eq!(found.len(), 1);
    test_harness::check_eq!(pinged, Some(Ok(PING)));
    test_harness::check_eq!(lines, ["started", "request role=c"]);
    finish(composition);
}

#[test_harness::test]
fn a_query_with_a_repeated_key_is_refused_and_the_next_one_is_served() {
    let composition = scenario(&[EXPORT]);
    let importer = composition.importer(&[probe_of(PROBE)]);
    let bytes = query(&[pair("role", "c"), pair("role", "c")]);
    let requirement = SessionRequirement {
        name: PROBE,
        protocol: probe::Protocol::ID,
    };

    let answer = startup_contract::Client::new(PortTransport::new(importer.handle()))
        .with_wait(COMPONENT_REPLY_TIMEOUT)
        .session(requirement, &bytes)
        .map(drop);
    let pinged = ping(&importer, PROBE);

    test_harness::check_eq!(answer, Err(CallError::Remote(SessionError::Refused)));
    test_harness::check_eq!(pinged, Ok(PING));
    finish(composition);
}

#[test_harness::test]
fn an_importer_finds_a_root_export_but_cannot_open_it() {
    let composition = scenario(&[]);
    let importer = composition.importer(&[crate::composition::Import {
        declaration: RequirementDeclaration::all(HARDWARE_REQUIREMENT, hardware::Protocol::ID),
        namespace_root: None,
    }]);

    let found = importer.found::<hardware::Protocol>(HARDWARE_REQUIREMENT);
    let opened = found.first().map(|found| {
        component_startup::open::<hardware::Protocol>(found, STARTUP_REQUEST_TIMEOUT).map(drop)
    });
    let direct =
        requested::<hardware::Protocol>(&importer, HARDWARE_REQUIREMENT, COMPONENT_REPLY_TIMEOUT);

    test_harness::check_eq!(found.len(), 1);
    test_harness::check_eq!(opened, Some(Err(CallError::Remote(SessionError::Refused))));
    test_harness::check_eq!(direct, Err(CallError::Remote(SessionError::Refused)));
    finish(composition);
}

#[test_harness::test]
fn a_finished_enumeration_keeps_the_results_it_gave() {
    let composition = scenario(&[EXPORT, NEIGHBOUR]);
    let importer = composition.importer(&[probe_of(PROBE)]);
    let enumeration = enumerated(&importer, PROBE);

    let found = drained(&enumeration);
    let after =
        component_startup::next(&enumeration, STARTUP_REQUEST_TIMEOUT).map(|next| next.is_none());
    drop(enumeration);
    let pinged = found.iter().all(|found| ping_found(found) == Ok(PING));

    test_harness::check_eq!(after, Ok(true));
    test_harness::check_eq!(found.len(), 2);
    test_harness::check!(pinged);
    finish(composition);
}

#[test_harness::test]
fn closing_one_of_two_enumerations_leaves_the_other() {
    let composition = scenario(&[EXPORT, NEIGHBOUR]);
    let importer = composition.importer(&[probe_of(PROBE)]);
    let first = enumerated(&importer, PROBE);
    let second = enumerated(&importer, PROBE);

    drop(first);
    let rest_count = drained(&second).len();

    test_harness::check_eq!(rest_count, 2);
    finish(composition);
}

#[test_harness::test]
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

    test_harness::check_eq!(sibling_closed, Ok(PING));
    test_harness::check_eq!(importer_closed, Ok(PING));
    finish(composition);
}

#[test_harness::test]
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

    test_harness::check!(enumeration_revoked);
    test_harness::check!(found_revoked);
    test_harness::check_eq!(foreign_pinged, Ok(PING));
    finish(composition);
}

#[test_harness::test]
fn repeated_searches_release_their_grants() {
    let composition = scenario(&[EXPORT, NEIGHBOUR]);
    let importer = composition.importer(&[probe_of(PROBE)]);

    let pinged = (0..SEARCH_CYCLES).all(|_| {
        let enumeration = enumerated(&importer, PROBE);
        let found = component_startup::next(&enumeration, STARTUP_REQUEST_TIMEOUT)
            .expect("next answered")
            .expect("a provider is found");
        ping_found(&found) == Ok(PING)
    });
    let found_count = importer.found::<probe::Protocol>(PROBE).len();

    test_harness::check!(pinged);
    test_harness::check_eq!(found_count, 2);
    finish(composition);
}
