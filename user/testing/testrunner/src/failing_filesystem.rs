//! Файловая система, которая отвечает отказом на каждую операцию.
//!
//! Сервер монтируется по требованию `FAILING_VIEW` и выдаёт окно на каждый отказ
//! `FileSystemError`: путь `/failing/<отказ>/...` получает сессию, все операции которой
//! отвечают этим отказом.

use alloc::format;
use core::num::NonZeroU64;

use component_startup::{SessionEvent, SessionEvents};
use filesystem_abi::{
    AccessMode, FileSystemError, IoBootstrap, NodeDescriptor, NodeInfo, NodeKind, OpenFlags,
    Opened, file_system,
};
use ipc::{IntoWireValue, OwnedIncoming, wire::Cap};
use kernel_objects::{Delivery, IssuedHandle, Port, Session, kind};
use runtime::{JoinHandle, spawn_owned};
use syscall::Rights;

use crate::{
    composition::Mounted,
    filesystem::{FAILING_VIEW, mount},
    fixture::JOIN_TIMEOUT,
};

const MOUNT_BADGE: u64 = 0x0600_0000;

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

pub(crate) struct FailingMount {
    mounted: Option<Mounted>,
    server: Option<JoinHandle>,
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

#[allow(clippy::unused_async_trait_impl)]
impl file_system::Service<Delivery> for Failing<'_> {
    fn start_io(
        &self,
        frame: &Delivery,
        reply: file_system::start_io::Reply<IssuedHandle<kind::Reply>>,
    ) {
        let _ = reply.send(Err::<
            IoBootstrap<IssuedHandle<kind::MemoryRegion>, IssuedHandle<kind::Signal>>,
            _,
        >(error_of(frame)));
    }

    fn take_io_region(
        &self,
        frame: &Delivery,
        _region: u64,
    ) -> impl IntoWireValue<Delivery, Read = Result<Cap<kind::MemoryRegion>, FileSystemError>> {
        Err::<IssuedHandle<kind::MemoryRegion>, _>(error_of(frame))
    }

    fn take_io_scratch(
        &self,
        frame: &Delivery,
    ) -> impl IntoWireValue<Delivery, Read = Result<Cap<kind::MemoryRegion>, FileSystemError>> {
        Err::<IssuedHandle<kind::MemoryRegion>, _>(error_of(frame))
    }

    async fn open(
        &self,
        frame: &Delivery,
        _path: &str,
        _mode: AccessMode,
        _flags: OpenFlags,
    ) -> Result<Opened, FileSystemError> {
        Err(error_of(frame))
    }

    fn close(&self, frame: &Delivery, _fd: NodeDescriptor) -> Result<u32, FileSystemError> {
        Err(error_of(frame))
    }

    async fn set_size(
        &self,
        frame: &Delivery,
        _fd: NodeDescriptor,
        _size: u64,
    ) -> Result<u32, FileSystemError> {
        Err(error_of(frame))
    }

    fn stat(&self, frame: &Delivery, _fd: NodeDescriptor) -> Result<NodeInfo, FileSystemError> {
        Err(error_of(frame))
    }

    fn rename(&self, frame: &Delivery, _from: &str, _to: &str) -> Result<u32, FileSystemError> {
        Err(error_of(frame))
    }

    fn unlink(
        &self,
        frame: &Delivery,
        _path: &str,
        _expected: NodeKind,
    ) -> Result<u32, FileSystemError> {
        Err(error_of(frame))
    }

    fn create_backed(
        &self,
        frame: &Delivery,
        _path: &str,
        _region: IssuedHandle<kind::MemoryRegion>,
        _size: u64,
    ) -> Result<u32, FileSystemError> {
        Err(error_of(frame))
    }

    async fn backing_memory(
        &self,
        frame: &Delivery,
        _fd: NodeDescriptor,
        reply: file_system::backing_memory::Reply<IssuedHandle<kind::Reply>>,
    ) {
        let _ = reply.send(Err::<IssuedHandle<kind::MemoryRegion>, _>(error_of(frame)));
    }

    fn open_root(
        &self,
        frame: &Delivery,
        path: &str,
    ) -> impl IntoWireValue<
        Delivery,
        Read = Result<ipc::wire::Session<file_system::Protocol>, FileSystemError>,
    > {
        if frame.badge != MOUNT_BADGE {
            return Err(error_of(frame));
        }

        self.window(path).map(Session::from_handle)
    }
}

impl Drop for FailingMount {
    fn drop(&mut self) {
        drop(self.mounted.take());
        if let Some(server) = self.server.take() {
            server
                .join(JOIN_TIMEOUT)
                .expect("failing server stops once its mount is gone");
        }
    }
}

pub(crate) fn mount_failing() -> FailingMount {
    let mount_badge = NonZeroU64::new(MOUNT_BADGE).expect("badge is non-zero");
    let failing = Port::create_service(Some(mount_badge)).expect("failing service");
    let server = failing.server;
    let thread = spawn_owned(move || serve_failing(&server)).expect("failing server starts");

    FailingMount {
        mounted: Some(mount(FAILING_VIEW, Session::from_handle(failing.client))),
        server: Some(thread),
    }
}

fn serve_failing(server: &Port) -> u32 {
    let executor = crate::executor::executor();
    let reactor = executor.reactor();
    let failing = Failing { server };
    let mut events = SessionEvents::new(server);

    executor.block_on(async {
        while let Some(event) = events.recv(&reactor).await.expect("failing receive") {
            match event {
                SessionEvent::Frame(frame) => {
                    let _ = file_system::dispatch(
                        &failing,
                        OwnedIncoming::new(frame).expect("frame fits"),
                    )
                    .await;
                }
                SessionEvent::Gone(MOUNT_BADGE) => return 0,
                SessionEvent::Gone(_) => {}
            }
        }

        0
    })
}

fn error_of(frame: &Delivery) -> FileSystemError {
    frame
        .badge
        .checked_sub(MOUNT_BADGE + 1)
        .and_then(|offset| usize::try_from(offset).ok())
        .and_then(|index| ERRORS.get(index).copied())
        .unwrap_or(FileSystemError::NotSupported)
}
