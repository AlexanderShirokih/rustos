//! Прогон стенда стандартной библиотеки.
//!
//! Тестовый родитель запускает программу на `std`, как любого ребёнка, со своей стартовой
//! сессией и журналом. Программа сообщает итог проверок кодом выхода, а подробности провала
//! уходят в журнал сообщением паники.

use alloc::{format, string::String};
use core::num::NonZeroU64;

use filesystem_abi::{
    AccessMode, DirCursor, FileSystemError, Listed, NodeDescriptor, NodeInfo, NodeKind, OpenFlags,
    Opened, PATH_MAX, Written, file_system, namespace,
};
use ipc::{
    IntoWireValue, Listener as _, Protocol as _, RecvError,
    wire::{Cap, MESSAGE_INLINE_MAX, Str},
};
use kernel_objects::{Delivery, IssuedHandle, Port, Session, kind};
use kernel_tests::kernel_test;
use managed_component::{Departure, Outcome};
use rootkeeper::relay::LogSessions as _;
use runtime::{PortTransport, Priority, Timeout, spawn_owned};
use syscall::Rights;

use crate::{
    filesystem::{FAILING_VIEW, mount, root_view},
    fixture::{JOIN_TIMEOUT, process_bytes, program},
    log_relay::{
        SELFTEST_BADGE, ScenarioLogSessions, SharedSourceNames, await_component_lines,
        start_component_relay, stop_component_relay,
    },
    parent::{Answer, Child, Parent},
};

const SELFTEST_ENTRY: &str = "/std-selftest/exec";

const INSTANCE_NAME: &str = "std-selftest";

const NAMESPACE_REQUIREMENT: &str = "namespace";

const PROCESS_TIMEOUT: Timeout = Timeout::from_secs(20);

const HEAP_SLACK_BYTES: u64 = 4 * 1024 * 1024;

const MOUNT_BADGE: u64 = 0x0600_0000;

const FAILING_STACK_BYTES: u64 = 0x4000;

const ERRORS: [FileSystemError; 17] = [
    FileSystemError::NotFound,
    FileSystemError::NotADirectory,
    FileSystemError::IsADirectory,
    FileSystemError::NotEmpty,
    FileSystemError::AlreadyExists,
    FileSystemError::InvalidPath,
    FileSystemError::BadDescriptor,
    FileSystemError::TooManyOpenFiles,
    FileSystemError::ReadOnly,
    FileSystemError::NoSpace,
    FileSystemError::CrossMount,
    FileSystemError::NoSession,
    FileSystemError::NoBuffer,
    FileSystemError::BufferTooSmall,
    FileSystemError::NotSupported,
    FileSystemError::Unresponsive,
    FileSystemError::WrongMode,
];

struct Failing<'a> {
    server: &'a Port,
}

impl Failing<'_> {
    fn window(&self, path: &str) -> Result<Port, FileSystemError> {
        let name = path.trim_start_matches('/').split('/').next().unwrap_or("");
        let index = ERRORS
            .iter()
            .position(|error| format!("{error:?}") == name)
            .ok_or(FileSystemError::NotFound)?;

        let badge = u64::try_from(index)
            .ok()
            .and_then(|index| NonZeroU64::new(MOUNT_BADGE + 1 + index))
            .ok_or(FileSystemError::NotFound)?;

        self.server
            .duplicate(Rights::WRITE | Rights::TRANSFER, Some(badge))
            .map_err(|_| FileSystemError::NoSession)
    }
}

impl file_system::Service<Delivery> for Failing<'_> {
    fn acquire_buffer(
        &mut self,
        frame: &Delivery,
        _size: u32,
    ) -> impl IntoWireValue<Delivery, Read = Result<Cap<kind::MemoryRegion>, FileSystemError>> {
        Err::<IssuedHandle<kind::MemoryRegion>, _>(error_of(frame))
    }

    fn open(
        &mut self,
        frame: &Delivery,
        _path: Str<'_, PATH_MAX>,
        _mode: AccessMode,
        _flags: OpenFlags,
    ) -> Result<Opened, FileSystemError> {
        Err(error_of(frame))
    }

    fn close(&mut self, frame: &Delivery, _fd: NodeDescriptor) -> Result<u32, FileSystemError> {
        Err(error_of(frame))
    }

    fn read(
        &mut self,
        frame: &Delivery,
        _fd: NodeDescriptor,
        _offset: u64,
        _len: u32,
    ) -> Result<u32, FileSystemError> {
        Err(error_of(frame))
    }

    fn write(
        &mut self,
        frame: &Delivery,
        _fd: NodeDescriptor,
        _offset: u64,
        _len: u32,
    ) -> Result<Written, FileSystemError> {
        Err(error_of(frame))
    }

    fn set_size(
        &mut self,
        frame: &Delivery,
        _fd: NodeDescriptor,
        _size: u64,
    ) -> Result<u32, FileSystemError> {
        Err(error_of(frame))
    }

    fn stat(&mut self, frame: &Delivery, _fd: NodeDescriptor) -> Result<NodeInfo, FileSystemError> {
        Err(error_of(frame))
    }

    fn list(
        &mut self,
        frame: &Delivery,
        _fd: NodeDescriptor,
        _from: DirCursor,
    ) -> Result<Listed, FileSystemError> {
        Err(error_of(frame))
    }

    fn rename(
        &mut self,
        frame: &Delivery,
        _from: Str<'_, PATH_MAX>,
        _to: Str<'_, PATH_MAX>,
    ) -> Result<u32, FileSystemError> {
        Err(error_of(frame))
    }

    fn unlink(
        &mut self,
        frame: &Delivery,
        _path: Str<'_, PATH_MAX>,
        _expected: NodeKind,
    ) -> Result<u32, FileSystemError> {
        Err(error_of(frame))
    }

    fn create_backed(
        &mut self,
        frame: &Delivery,
        _path: Str<'_, PATH_MAX>,
        _region: IssuedHandle<kind::MemoryRegion>,
        _size: u64,
    ) -> Result<u32, FileSystemError> {
        Err(error_of(frame))
    }

    fn backing_memory(
        &mut self,
        frame: &Delivery,
        _fd: NodeDescriptor,
    ) -> impl IntoWireValue<Delivery, Read = Result<Cap<kind::MemoryRegion>, FileSystemError>> {
        Err::<IssuedHandle<kind::MemoryRegion>, _>(error_of(frame))
    }

    fn open_root(
        &mut self,
        _frame: &Delivery,
        path: Str<'_, PATH_MAX>,
    ) -> impl IntoWireValue<
        Delivery,
        Read = Result<ipc::wire::Session<file_system::Protocol>, FileSystemError>,
    > {
        self.window(path.as_str()).map(Session::from_handle)
    }
}

#[kernel_test]
fn std_selftest_program_passes_its_checks() {
    let mount_badge = NonZeroU64::new(MOUNT_BADGE).expect("badge is non-zero");
    let failing = Port::create_service(Some(mount_badge)).expect("failing service");
    let server = failing.server;
    let failing_server = spawn_owned(
        move || serve_failing(&server),
        FAILING_STACK_BYTES,
        Priority::new(1),
    )
    .expect("failing server starts");

    let mounted = mount(FAILING_VIEW, Session::from_handle(failing.client));

    start_component_relay(SharedSourceNames);
    let log = ScenarioLogSessions
        .session(SELFTEST_BADGE)
        .expect("selftest log session");
    let budget = process_bytes(&program(SELFTEST_ENTRY)) + HEAP_SLACK_BYTES;
    let parent = Parent::start();

    let selftest = parent.add(
        Child::new(SELFTEST_ENTRY, INSTANCE_NAME)
            .log(Answer::log(log))
            .requirement(
                NAMESPACE_REQUIREMENT,
                Answer::Transfer {
                    protocol: namespace::Protocol::ID,
                    source: Some(root_view()),
                },
            )
            .budget(budget),
    );

    kernel_tests::kassert_eq!(
        parent.departure(PROCESS_TIMEOUT),
        Some(Departure {
            child: selftest,
            outcome: Outcome::Exit(0),
        })
    );

    let lines = await_component_lines(INSTANCE_NAME, 3);
    stop_component_relay();
    kernel_tests::kassert_eq!(lines.first().map(String::as_str), Some("selftest started"));
    kernel_tests::kassert_eq!(lines.last().map(String::as_str), Some("checks passed"));

    drop(mounted);
    failing_server
        .join(JOIN_TIMEOUT)
        .expect("failing server stops once its mount is gone");
}

fn serve_failing(server: &Port) -> u32 {
    let transport = PortTransport::new(server);
    let mut failing = Failing { server };
    let mut bytes = [0u8; MESSAGE_INLINE_MAX];

    loop {
        match transport.recv(&mut bytes, Timeout::INFINITE) {
            Ok(frame) => {
                let _ = file_system::dispatch(&mut failing, frame);
            }
            Err(RecvError::SenderGone(MOUNT_BADGE) | RecvError::Closed) => return 0,
            Err(_) => {}
        }
    }
}

fn error_of(frame: &Delivery) -> FileSystemError {
    frame
        .badge
        .checked_sub(MOUNT_BADGE + 1)
        .and_then(|offset| usize::try_from(offset).ok())
        .and_then(|index| ERRORS.get(index).copied())
        .unwrap_or(FileSystemError::NotSupported)
}
