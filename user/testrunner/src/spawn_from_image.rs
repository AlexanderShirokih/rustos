//! E2E спавна из реального userland-образа.

use bootstrap::BootstrapClient;
use ipc::wire::IpcError;
use kernel_tests::kernel_test;
use runtime::{MemoryRegion, OwnedHandle, PortTransport, Resource, Signal, Timeout};
use syscall::SIGNALED;
use userland::{EntryView, SegmentView, USERLAND_PAGE_SIZE};
use userland_image::decode;
use userland_loader::{launch_entry, load_entry};

use crate::startup_fixture_protocol::{STARTUP_VALUE, StartupFixtureClient};

const CHILD_WAIT_TIMEOUT_NS: u64 = 500_000_000;
const CALL_TIMEOUT_NS: u64 = 2_000_000_000;
const STARTUP_SLACK_PAGES: u64 = 8;
const STARTUP_FAILURE_ATTEMPTS: usize = 16;
const PROCESS_CLEANUP_RETRY_ATTEMPTS: usize = 64;
const PROCESS_CLEANUP_RETRY_NS: u64 = 1_000_000;

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
    let client = BootstrapClient::new(PortTransport::client(crate::bootstrap_handle()));

    let cap = client
        .acquire_userland_image()
        .expect("acquire_userland_image call")
        .expect("vend ok");

    let region = MemoryRegion::from_handle(OwnedHandle::adopt(cap));
    let mapping = region.map_full_read_only().expect("map image read-only");
    let image = decode(mapping.as_bytes()).expect("decode acquired image");
    let child = image
        .entry("/boot/startup-fixture")
        .expect("image carries a child entry")
        .program()
        .expect("child entry is a program");

    let resource = Resource::self_resource().expect("self metering resource");
    let process = launch_entry(&child, &resource, |startup| -> Result<(), StartupError> {
        StartupFixtureClient::new(PortTransport::client(startup.handle())).start(STARTUP_VALUE)?;
        Ok(())
    })
    .expect("startup frame delivered");

    process
        .join(Timeout::from_ns(CHILD_WAIT_TIMEOUT_NS))
        .expect("child terminates");

    kernel_tests::kassert_eq!(process.exit_code().expect("exit code"), 0);
}

#[kernel_test]
fn failed_startup_returns_child_budget() {
    let client = BootstrapClient::new(PortTransport::client(crate::bootstrap_handle()));

    let cap = client
        .acquire_userland_image()
        .expect("acquire_userland_image call")
        .expect("vend ok");

    let region = MemoryRegion::from_handle(OwnedHandle::adopt(cap));
    let mapping = region.map_full_read_only().expect("map image read-only");
    let image = decode(mapping.as_bytes()).expect("decode acquired image");
    let child = image
        .entry("/boot/startup-fixture")
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

/// Сервер принимает `call` и уходит, не ответив: вызыватель обязан быть
/// разбужен отказом, а не остаться припаркованным.
#[kernel_test]
fn caller_wakes_when_server_dies_holding_reply() {
    use runtime::{Error, Port};
    use syscall::SyscallError;

    let client = BootstrapClient::new(PortTransport::client(crate::bootstrap_handle()));
    let cap = client
        .acquire_userland_image()
        .expect("acquire_userland_image call")
        .expect("vend ok");
    let region = MemoryRegion::from_handle(OwnedHandle::adopt(cap));
    let mapping = region.map_full_read_only().expect("map image read-only");
    let image = decode(mapping.as_bytes()).expect("decode acquired image");
    let child = image
        .entry("/boot/reply-holder")
        .expect("image carries the holder")
        .program()
        .expect("holder record carries a program");

    let service = Port::create_service(0).expect("port create");

    let resource = Resource::self_resource().expect("self metering resource");
    let _process = load_entry(&child, &resource, service.server).expect("spawn holder from image");

    let mut out = [0u8; 8];
    // Кадр уже принят, поэтому вызыватель снят с очереди порта: его будит снос
    // reply-хэндла вместе с таблицей ушедшего сервера, а не пустое множество
    // получателей. Тайм-аут страхует от вечной парковки, исход - `PeerClosed`.
    let result = service
        .client
        .call_bytes(b"x", &mut out, Timeout::from_ns(CALL_TIMEOUT_NS));
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
        match launch_entry(program, budget, |_| Err(StartupError::Injected)) {
            Err(StartupError::Runtime(runtime::Error::Syscall(
                syscall::SyscallError::ResourceExhausted,
            ))) => {
                let _ = timer.wait(SIGNALED, Timeout::from_ns(PROCESS_CLEANUP_RETRY_NS));
            }
            Err(StartupError::Injected) => return,
            Err(error) => panic!("unexpected child launch failure: {error:?}"),
            Ok(_) => panic!("injected startup failure succeeded"),
        }
    }
    panic!("terminated child budget was not returned")
}
