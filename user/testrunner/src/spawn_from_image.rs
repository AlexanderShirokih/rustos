//! E2E спавна из реального userland-образа.

use bootstrap_abi::bootstrap;
use ipc::wire::IpcError;
use kernel_tests::kernel_test;
use runtime::{MemoryRegion, OwnedHandle, PortTransport, Resource, Signal, Timeout};
use syscall::{Rights, SIGNALED, SyscallError};
use userland::{Entry, EntryView, Segment, SegmentPermissions, SegmentView, USERLAND_PAGE_SIZE};
use userland_image::{component_name, decode};
use userland_loader::{launch_entry, load_entry};

use crate::startup_fixture_protocol::{STARTUP_VALUE, startup_fixture};

const STARTUP_FIXTURE: &str = "/boot/startup-fixture/program";
const REPLY_HOLDER: &str = "/boot/reply-holder/program";

const CHILD_WAIT_TIMEOUT: Timeout = Timeout::from_millis(500);
const CALL_TIMEOUT: Timeout = Timeout::from_secs(2);
const STARTUP_SLACK_PAGES: u64 = 8;
const STARTUP_FAILURE_ATTEMPTS: usize = 16;
const PROCESS_CLEANUP_RETRY_ATTEMPTS: usize = 64;
const PROCESS_CLEANUP_RETRY: Timeout = Timeout::from_millis(1);
const PROCESS_CREATE_ROLLBACK_ATTEMPTS: usize = 80;

#[derive(Debug)]
enum StartupError {
    Runtime(runtime::Error),
    Ipc,
    Injected,
}

impl From<runtime::Error> for StartupError {
    fn from(error: runtime::Error) -> Self {
        Self::Runtime(error)
    }
}

impl From<IpcError> for StartupError {
    fn from(_error: IpcError) -> Self {
        Self::Ipc
    }
}

#[kernel_test]
fn launch_child_delivers_startup_frame() {
    let client = bootstrap::Client::new(PortTransport::client(crate::bootstrap_handle()));

    let cap = client
        .acquire_userland_image()
        .expect("acquire_userland_image call")
        .expect("vend ok");

    let region = MemoryRegion::from_handle(OwnedHandle::adopt(cap));
    let mapping = region.map_full_read_only().expect("map image read-only");
    let image = decode(mapping.as_bytes()).expect("decode acquired image");
    let child = image
        .entry(STARTUP_FIXTURE)
        .expect("image carries a child entry")
        .program()
        .expect("child entry is a program");

    let resource = Resource::self_resource().expect("self metering resource");
    let process = launch_entry(
        &child,
        Some(component_name(STARTUP_FIXTURE)),
        &resource,
        |startup| -> Result<(), StartupError> {
            startup_fixture::Client::new(PortTransport::client(startup.handle()))
                .start(STARTUP_VALUE)?;
            Ok(())
        },
    )
    .expect("startup frame delivered");

    process.join(CHILD_WAIT_TIMEOUT).expect("child terminates");

    kernel_tests::kassert_eq!(process.exit_code().expect("exit code"), 0);
}

#[kernel_test]
fn failed_startup_returns_child_budget() {
    let client = bootstrap::Client::new(PortTransport::client(crate::bootstrap_handle()));

    let cap = client
        .acquire_userland_image()
        .expect("acquire_userland_image call")
        .expect("vend ok");

    let region = MemoryRegion::from_handle(OwnedHandle::adopt(cap));
    let mapping = region.map_full_read_only().expect("map image read-only");
    let image = decode(mapping.as_bytes()).expect("decode acquired image");
    let child = image
        .entry(STARTUP_FIXTURE)
        .expect("image carries a child entry")
        .program()
        .expect("child entry is a program");

    let parent = Resource::self_resource().expect("self metering resource");
    let budget = parent
        .split(process_pages(&child) + STARTUP_SLACK_PAGES)
        .expect("split startup budget");
    let scheduler_turn = Signal::create().expect("termination timer");

    for _ in 0..STARTUP_FAILURE_ATTEMPTS {
        fail_startup_when_budget_returns(&child, &budget, &scheduler_turn);
    }
}

#[kernel_test]
fn load_image_error_reclaims_each_created_process() {
    const BASE: u64 = 0x4000_0000;
    let segments = [
        Segment {
            va_base: BASE,
            mem_size: USERLAND_PAGE_SIZE,
            permissions: SegmentPermissions::ReadExecute,
            bytes: &[],
        },
        Segment {
            va_base: BASE,
            mem_size: USERLAND_PAGE_SIZE,
            permissions: SegmentPermissions::ReadWrite,
            bytes: &[],
        },
    ];
    let invalid = Entry {
        path: "/boot/load-image-rollback",
        entry_va: BASE,
        stack_size: USERLAND_PAGE_SIZE,
        segments: &segments,
    };
    let resource = Resource::self_resource().expect("self metering resource");

    for _ in 0..PROCESS_CREATE_ROLLBACK_ATTEMPTS {
        let initial = Signal::create().expect("initial capability").into_handle();
        kernel_tests::kassert!(matches!(
            load_entry(&invalid, None, &resource, initial),
            Err(runtime::Error::Syscall(SyscallError::InvalidArgument))
        ));
    }
}

#[kernel_test]
fn start_error_reclaims_each_loaded_process() {
    let client = bootstrap::Client::new(PortTransport::client(crate::bootstrap_handle()));
    let cap = client
        .acquire_userland_image()
        .expect("acquire_userland_image call")
        .expect("vend ok");
    let region = MemoryRegion::from_handle(OwnedHandle::adopt(cap));
    let mapping = region.map_full_read_only().expect("map image read-only");
    let image = decode(mapping.as_bytes()).expect("decode acquired image");
    let child = image
        .entry(STARTUP_FIXTURE)
        .expect("image carries a child entry")
        .program()
        .expect("child entry is a program");

    let root = Resource::self_resource().expect("self metering resource");
    let write_only = Resource::from_handle(
        root.handle()
            .duplicate(Rights::WRITE, None)
            .expect("resource without duplicate right"),
    );

    for _ in 0..PROCESS_CREATE_ROLLBACK_ATTEMPTS {
        let initial = Signal::create().expect("initial capability").into_handle();
        kernel_tests::kassert!(matches!(
            load_entry(
                &child,
                Some(component_name(STARTUP_FIXTURE)),
                &write_only,
                initial
            ),
            Err(runtime::Error::Syscall(SyscallError::AccessDenied))
        ));
    }
}

/// Сервер принимает `call` и уходит, не ответив: вызыватель обязан быть
/// разбужен отказом.
#[kernel_test]
fn caller_wakes_when_server_dies_holding_reply() {
    use runtime::{Error, Port};
    use syscall::SyscallError;

    let client = bootstrap::Client::new(PortTransport::client(crate::bootstrap_handle()));
    let cap = client
        .acquire_userland_image()
        .expect("acquire_userland_image call")
        .expect("vend ok");
    let region = MemoryRegion::from_handle(OwnedHandle::adopt(cap));
    let mapping = region.map_full_read_only().expect("map image read-only");
    let image = decode(mapping.as_bytes()).expect("decode acquired image");
    let child = image
        .entry(REPLY_HOLDER)
        .expect("image carries the holder")
        .program()
        .expect("holder record carries a program");

    let service = Port::create_service(None).expect("port create");

    let resource = Resource::self_resource().expect("self metering resource");
    let _process = load_entry(
        &child,
        Some(component_name(REPLY_HOLDER)),
        &resource,
        service.server,
    )
    .expect("spawn holder from image");

    let mut out = [0u8; 8];
    // Кадр уже принят, поэтому вызыватель снят с очереди порта: его будит снос
    // reply-хэндла вместе с таблицей ушедшего сервера, а не пустое множество
    // получателей. Тайм-аут страхует от вечной парковки, исход - `PeerClosed`.
    let result = service.client.call_bytes(b"x", &mut out, CALL_TIMEOUT);
    kernel_tests::kassert!(matches!(
        result,
        Err(Error::Syscall(SyscallError::PeerClosed))
    ));
}

fn process_pages<'a, E: EntryView<'a>>(program: &E) -> u64 {
    let segments = program
        .segments()
        .map(|segment| segment.mem_size().div_ceil(USERLAND_PAGE_SIZE))
        .sum::<u64>();
    segments + program.stack_size().div_ceil(USERLAND_PAGE_SIZE)
}

fn fail_startup_when_budget_returns<'a, E: EntryView<'a>>(
    program: &E,
    budget: &Resource,
    timer: &Signal,
) {
    for _ in 0..PROCESS_CLEANUP_RETRY_ATTEMPTS {
        let name = component_name(program.path());
        match launch_entry(program, Some(name), budget, |_| Err(StartupError::Injected)) {
            Err(StartupError::Runtime(runtime::Error::Syscall(
                syscall::SyscallError::ResourceExhausted,
            ))) => {
                let _ = timer.wait(SIGNALED, PROCESS_CLEANUP_RETRY);
            }
            Err(StartupError::Injected) => return,
            Err(error) => panic!("unexpected child launch failure: {error:?}"),
            Ok(_) => panic!("injected startup failure succeeded"),
        }
    }
    panic!("terminated child budget was not returned")
}
