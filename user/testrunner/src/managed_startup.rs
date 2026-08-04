//! QEMU-сценарии managed startup rootkeeper.

use bootstrap::BootstrapClient;
use ipc::{IntoWireHandle, wire::IpcError};
use kernel_tests::kernel_test;
use rootkeeper::{
    BoundedStartupTransport, ManagedComponent, ManagedFailure, ManagedStartupError, ManagedStatus,
};
use runtime::{
    Error, MemoryRegion, OwnedHandle, PortTransport, ReadOnlyMapping, Resource, Signal, Timeout,
};
use syscall::{Rights, SIGNALED, SyscallError};
use userland::{EntryView, SegmentView, USERLAND_PAGE_SIZE};
use userland_image::{DecodedProgram, decode};

use crate::managed_fixture_protocol::ManagedFixtureClient;

const FIXTURE_ENTRY: &str = "/boot/managed-startup-fixture";
const HUNG_ENTRY: &str = "/boot/managed-startup-hung";
const CLOSED_ENTRY: &str = "/boot/managed-startup-closed";
const DELIVERY_TIMEOUT: Timeout = Timeout::from_secs(1);
const HUNG_DELIVERY_TIMEOUT: Timeout = Timeout::from_millis(5);
const PROCESS_TIMEOUT: Timeout = Timeout::from_secs(1);
const PROCESS_SLACK_PAGES: u64 = 4;
const ROLLBACK_ATTEMPTS: u32 = 64;
const RACE_ATTEMPTS: u32 = 64;
const RACE_TIMEOUT: Timeout = Timeout::from_micros(50);
const FAILURE_EXIT_CODE: u32 = 37;
const DROP_BUDGET_RETURN_TIMEOUT: Timeout = Timeout::from_secs(5);

struct FixtureRoots {
    authority: Signal,
}

struct TrackedAuthority<'a> {
    handle: Option<OwnedHandle>,
    sender_cleanup: &'a Signal,
    relinquished: bool,
}

struct ImageMapping {
    mapping: ReadOnlyMapping,
    _region: MemoryRegion,
}

impl FixtureRoots {
    fn create() -> Result<Self, ManagedStartupError> {
        Ok(Self {
            authority: Signal::create()?,
        })
    }

    fn child_authority(&self) -> Result<OwnedHandle, ManagedStartupError> {
        self.authority
            .handle()
            .duplicate(Rights::READ | Rights::WRITE | Rights::TRANSFER, None)
            .map_err(ManagedStartupError::from)
    }

    fn from_observer(observer: &Signal) -> Result<Self, ManagedStartupError> {
        let rights = Rights::READ | Rights::WRITE | Rights::TRANSFER | Rights::DUPLICATE;
        Ok(Self {
            authority: Signal::from_handle(observer.handle().duplicate(rights, None)?),
        })
    }
}

impl<'a> TrackedAuthority<'a> {
    fn new(handle: OwnedHandle, sender_cleanup: &'a Signal) -> Self {
        Self {
            handle: Some(handle),
            sender_cleanup,
            relinquished: false,
        }
    }
}

impl Drop for TrackedAuthority<'_> {
    fn drop(&mut self) {
        if !self.relinquished {
            self.sender_cleanup
                .notify_all()
                .expect("mark sender-side capability cleanup");
        }
    }
}

impl IntoWireHandle for TrackedAuthority<'_> {
    fn wire_id(&self) -> u32 {
        self.handle
            .as_ref()
            .expect("tracked authority is present")
            .as_raw()
            .raw()
    }

    fn relinquish(mut self) {
        self.relinquished = true;
        let handle = self.handle.take().expect("tracked authority is present");
        let _ = handle.into_raw();
    }
}

#[kernel_test]
fn managed_startup_times_out_before_recv_and_repeatedly_rolls_back() {
    let mapping = image_mapping();
    let program = image_program(&mapping, HUNG_ENTRY);
    let root = Resource::self_resource().expect("self metering resource");
    let budget_pages = process_pages(&program) + PROCESS_SLACK_PAGES;
    let exact_parent = root
        .split(budget_pages)
        .expect("reserve exact repeated budget");

    for _ in 0..ROLLBACK_ATTEMPTS {
        let mut component = ManagedComponent::<FixtureRoots>::new(&exact_parent, budget_pages)
            .expect("reserve the component budget");
        let result = component.launch(
            &program,
            HUNG_DELIVERY_TIMEOUT,
            FixtureRoots::create,
            |transport, roots| send_startup(transport, roots, 0, false),
        );

        kernel_tests::kassert_eq!(
            result,
            Err(ManagedStartupError::Delivery(IpcError::Timeout))
        );
        kernel_tests::kassert_eq!(component.status(), ManagedStatus::Failed);
        kernel_tests::kassert_eq!(
            component.failure(),
            Some(ManagedFailure::Startup(ManagedStartupError::Delivery(
                IpcError::Timeout
            )))
        );
    }
}

#[kernel_test]
fn managed_startup_peer_close_repeatedly_rolls_back() {
    let mapping = image_mapping();
    let program = image_program(&mapping, CLOSED_ENTRY);
    let root = Resource::self_resource().expect("self metering resource");
    let budget_pages = process_pages(&program) + PROCESS_SLACK_PAGES;
    let exact_parent = root
        .split(budget_pages)
        .expect("reserve exact repeated budget");

    for _ in 0..ROLLBACK_ATTEMPTS {
        let mut component = ManagedComponent::<FixtureRoots>::new(&exact_parent, budget_pages)
            .expect("reserve the component budget");
        let result = component.launch(
            &program,
            DELIVERY_TIMEOUT,
            FixtureRoots::create,
            |transport, roots| send_startup(transport, roots, 0, false),
        );

        kernel_tests::kassert_eq!(
            result,
            Err(ManagedStartupError::Delivery(IpcError::PeerClosed))
        );
        kernel_tests::kassert_eq!(component.status(), ManagedStatus::Failed);
        kernel_tests::kassert_eq!(
            component.failure(),
            Some(ManagedFailure::Startup(ManagedStartupError::Delivery(
                IpcError::PeerClosed
            )))
        );
    }
}

#[kernel_test]
fn managed_startup_observes_nonzero_exit_after_running() {
    let mapping = image_mapping();
    let program = image_program(&mapping, FIXTURE_ENTRY);
    let parent = Resource::self_resource().expect("self metering resource");
    let mut component = ManagedComponent::<FixtureRoots>::new(
        &parent,
        process_pages(&program) + PROCESS_SLACK_PAGES,
    )
    .expect("reserve the component budget");

    component
        .launch(
            &program,
            DELIVERY_TIMEOUT,
            FixtureRoots::create,
            |transport, roots| send_startup(transport, roots, FAILURE_EXIT_CODE, false),
        )
        .expect("startup frame delivered");

    kernel_tests::kassert_eq!(component.status(), ManagedStatus::Running);
    wait_for_authority(&component);
    component
        .observe_exit(PROCESS_TIMEOUT)
        .expect("observe fixture exit");
    kernel_tests::kassert_eq!(component.status(), ManagedStatus::Failed);
    kernel_tests::kassert_eq!(
        component.failure(),
        Some(ManagedFailure::Exit(FAILURE_EXIT_CODE))
    );
}

#[kernel_test]
fn managed_startup_observes_zero_exit_after_running() {
    let mapping = image_mapping();
    let program = image_program(&mapping, FIXTURE_ENTRY);
    let parent = Resource::self_resource().expect("self metering resource");
    let mut component = ManagedComponent::<FixtureRoots>::new(
        &parent,
        process_pages(&program) + PROCESS_SLACK_PAGES,
    )
    .expect("reserve the component budget");

    component
        .launch(
            &program,
            DELIVERY_TIMEOUT,
            FixtureRoots::create,
            |transport, roots| send_startup(transport, roots, 0, false),
        )
        .expect("startup frame delivered");

    kernel_tests::kassert_eq!(component.status(), ManagedStatus::Running);
    wait_for_authority(&component);
    component
        .observe_exit(PROCESS_TIMEOUT)
        .expect("observe fixture exit");
    kernel_tests::kassert_eq!(component.status(), ManagedStatus::Exited);
    kernel_tests::kassert_eq!(component.failure(), None);
}

#[kernel_test]
fn managed_startup_timeout_rendezvous_race_has_one_ownership_outcome() {
    let mapping = image_mapping();
    let program = image_program(&mapping, FIXTURE_ENTRY);
    let parent = Resource::self_resource().expect("self metering resource");
    let budget_pages = process_pages(&program) + PROCESS_SLACK_PAGES;

    for _ in 0..RACE_ATTEMPTS {
        let observer = Signal::create().expect("independent observer root");
        let sender_cleanup = Signal::create().expect("sender cleanup observer");
        let mut component = ManagedComponent::<FixtureRoots>::new(&parent, budget_pages)
            .expect("reserve the component budget");
        let result = component.launch(
            &program,
            RACE_TIMEOUT,
            || FixtureRoots::from_observer(&observer),
            |transport, roots| {
                let authority = TrackedAuthority::new(roots.child_authority()?, &sender_cleanup);
                ManagedFixtureClient::new(transport)
                    .start(authority, 0, false)
                    .map_err(ManagedStartupError::from)
            },
        );

        match result {
            Ok(()) => {
                kernel_tests::kassert_eq!(component.status(), ManagedStatus::Running);
                kernel_tests::kassert!(matches!(
                    sender_cleanup.wait(SIGNALED, Timeout::POLL),
                    Err(Error::Syscall(SyscallError::Timeout))
                ));
                observer
                    .wait(SIGNALED, PROCESS_TIMEOUT)
                    .expect("transferred authority was signaled");
                component
                    .observe_exit(PROCESS_TIMEOUT)
                    .expect("observe successful race fixture");
                kernel_tests::kassert_eq!(component.status(), ManagedStatus::Exited);
            }
            Err(ManagedStartupError::Delivery(IpcError::Timeout)) => {
                kernel_tests::kassert_eq!(component.status(), ManagedStatus::Failed);
                sender_cleanup
                    .wait(SIGNALED, Timeout::POLL)
                    .expect("timed-out capability was closed by sender RAII");
                kernel_tests::kassert!(matches!(
                    observer.wait(SIGNALED, Timeout::POLL),
                    Err(Error::Syscall(SyscallError::Timeout))
                ));
            }
            Err(error) => panic!("unexpected race outcome: {error:?}"),
        }
    }
}

#[kernel_test]
fn managed_startup_rejects_infinite_send_timeout() {
    let mapping = image_mapping();
    let program = image_program(&mapping, FIXTURE_ENTRY);
    let parent = Resource::self_resource().expect("self metering resource");
    let mut component = ManagedComponent::<FixtureRoots>::new(
        &parent,
        process_pages(&program) + PROCESS_SLACK_PAGES,
    )
    .expect("reserve the component budget");

    let result = component.launch(&program, Timeout::INFINITE, FixtureRoots::create, |_, _| {
        panic!("infinite timeout must be rejected before startup")
    });

    kernel_tests::kassert_eq!(result, Err(ManagedStartupError::InfiniteTimeout));
    kernel_tests::kassert_eq!(component.status(), ManagedStatus::Failed);
}

#[kernel_test]
fn dropping_running_component_terminates_child_and_returns_budget() {
    let mapping = image_mapping();
    let program = image_program(&mapping, FIXTURE_ENTRY);
    let root = Resource::self_resource().expect("self metering resource");
    let budget_pages = process_pages(&program) + PROCESS_SLACK_PAGES;
    let exact_parent = root
        .split(budget_pages)
        .expect("reserve exact managed budget");
    let mut component = ManagedComponent::<FixtureRoots>::new(&exact_parent, budget_pages)
        .expect("reserve the component budget");

    component
        .launch(
            &program,
            DELIVERY_TIMEOUT,
            FixtureRoots::create,
            |transport, roots| send_startup(transport, roots, 0, true),
        )
        .expect("startup frame delivered");
    wait_for_authority(&component);

    drop(component);
    wait_for_budget_return(&exact_parent, budget_pages);
}

fn send_startup(
    transport: BoundedStartupTransport,
    roots: &FixtureRoots,
    exit_code: u32,
    keep_running: bool,
) -> Result<(), ManagedStartupError> {
    let authority = roots.child_authority()?;
    ManagedFixtureClient::new(transport)
        .start(authority, exit_code, keep_running)
        .map_err(ManagedStartupError::from)
}

fn wait_for_authority(component: &ManagedComponent<FixtureRoots>) {
    component
        .running()
        .expect("component owns running instance")
        .roots()
        .authority
        .wait(SIGNALED, PROCESS_TIMEOUT)
        .expect("fixture used transferred authority");
}

fn image_mapping() -> ImageMapping {
    let client = BootstrapClient::new(PortTransport::client(crate::bootstrap_handle()));
    let cap = client
        .acquire_userland_image()
        .expect("acquire_userland_image call")
        .expect("vend ok");
    let region = MemoryRegion::from_handle(OwnedHandle::adopt(cap));
    let mapping = region.map_full_read_only().expect("map image read-only");
    ImageMapping {
        mapping,
        _region: region,
    }
}

fn image_program<'a>(image: &'a ImageMapping, path: &str) -> DecodedProgram<'a> {
    decode(image.mapping.as_bytes())
        .expect("decode acquired image")
        .entry(path)
        .expect("image carries managed fixture")
        .program()
        .expect("managed fixture entry is a program")
}

fn process_pages<'a, E: EntryView<'a>>(program: &E) -> u64 {
    let segments = program
        .segments()
        .map(|segment| segment.mem_size().div_ceil(USERLAND_PAGE_SIZE))
        .sum::<u64>();
    segments + program.stack_size().div_ceil(USERLAND_PAGE_SIZE)
}

fn wait_for_budget_return(resource: &Resource, budget_pages: u64) {
    resource
        .split_when_available(budget_pages, DROP_BUDGET_RETURN_TIMEOUT)
        .expect("managed budget returned");
}
