//! Управляющий порт: дескрипторы, метаданные и выдача capability файлового обмена.

use alloc::rc::Rc;

use filesystem::NodeId;
use filesystem_abi::{
    AccessMode, FileSystemError, NodeDescriptor, NodeInfo, NodeKind, OpenFlags, Opened, PATH_MAX,
    file_system,
};
use ipc::{IntoWireValue, ServiceOwner};
use kernel_objects::{Delivery, IssuedHandle, kind};

use crate::{
    io::{Exchange, Lease},
    server::Shared,
    service::{Serving, client_of, scoped},
};

type Debt = IssuedHandle<kind::Reply>;

#[derive(Clone)]
pub(crate) struct Control {
    pub(crate) shared: Rc<Shared>,
    pub(crate) volume: usize,
}

impl ServiceOwner for Control {
    type Service = Self;

    fn service(&self) -> &Self {
        self
    }
}

impl Control {
    fn serve<T>(
        &self,
        operation: impl FnOnce(&mut Serving<'_>) -> Result<T, FileSystemError>,
    ) -> Result<T, FileSystemError> {
        self.shared.with_state(self.volume, |state| {
            operation(&mut Serving {
                state,
                service: &self.shared.service,
            })
        })
    }

    fn path_node(
        &self,
        delivery: &Delivery,
        path: &str,
    ) -> Result<Option<NodeId>, FileSystemError> {
        let client = client_of(delivery)?;
        self.shared.with_state(self.volume, |state| {
            let mut full = [0; PATH_MAX];
            let scope = &state.sessions.get_mut(client)?.attachment().scope;
            let path = scoped(scope, path, &mut full)?;
            match state.backend.lookup(path) {
                Ok(node) => Ok(Some(node)),
                Err(FileSystemError::NotFound) => Ok(None),
                Err(error) => Err(error),
            }
        })
    }

    fn exchange<T>(
        &self,
        delivery: &Delivery,
        operation: impl FnOnce(&Exchange) -> Result<T, FileSystemError>,
    ) -> Result<T, FileSystemError> {
        let client = client_of(delivery)?;
        self.shared.with_state(self.volume, |state| {
            operation(
                state
                    .sessions
                    .get_mut(client)?
                    .attachment()
                    .io
                    .as_ref()
                    .ok_or(FileSystemError::NoBuffer)?,
            )
        })
    }
}

#[allow(clippy::unused_async_trait_impl)]
impl file_system::Service<Delivery> for Control {
    fn start_io(&self, delivery: &Delivery, reply: file_system::start_io::Reply<Debt>) {
        let client = client_of(delivery);
        let started = client.and_then(|client| Exchange::start(&self.shared, self.volume, client));
        let installed = started.is_ok();

        if reply.send(started).is_err()
            && installed
            && let Ok(client) = client
        {
            let exchange = self
                .shared
                .with_state(self.volume, |state| {
                    Ok(state.sessions.get_mut(client)?.attachment_mut().io.take())
                })
                .ok()
                .flatten();
            drop(exchange);
        }
    }

    fn take_io_region(
        &self,
        delivery: &Delivery,
        region: u64,
    ) -> impl IntoWireValue<Delivery, Read = Result<ipc::wire::Cap<kind::MemoryRegion>, FileSystemError>>
    {
        self.exchange(delivery, |exchange| exchange.claim(region))
    }

    fn take_io_scratch(
        &self,
        delivery: &Delivery,
    ) -> impl IntoWireValue<Delivery, Read = Result<ipc::wire::Cap<kind::MemoryRegion>, FileSystemError>>
    {
        self.exchange(delivery, Exchange::scratch)
    }

    async fn open(
        &self,
        delivery: &Delivery,
        path: &str,
        mode: AccessMode,
        flags: OpenFlags,
    ) -> Result<Opened, FileSystemError> {
        if !flags.contains(OpenFlags::TRUNCATE) {
            return self.serve(|service| service.open(delivery, path, mode, flags));
        }

        loop {
            let Some(node) = self.path_node(delivery, path)? else {
                return self.serve(|service| service.open(delivery, path, mode, flags));
            };

            let lease = Lease::node(self.shared.clone(), self.volume, node)?;
            let _locked = lease.lock.lock().await;

            if self.path_node(delivery, path)? == Some(node) {
                return self.serve(|service| service.open(delivery, path, mode, flags));
            }
        }
    }

    fn close(&self, delivery: &Delivery, fd: NodeDescriptor) -> Result<u32, FileSystemError> {
        self.serve(|service| service.close(delivery, fd))
    }

    async fn set_size(
        &self,
        delivery: &Delivery,
        fd: NodeDescriptor,
        size: u64,
    ) -> Result<u32, FileSystemError> {
        let lease = Lease::descriptor(self.shared.clone(), self.volume, client_of(delivery)?, fd)?;
        let _locked = lease.lock.lock().await;
        self.serve(|service| service.set_size(delivery, fd, size))
    }

    fn stat(&self, delivery: &Delivery, fd: NodeDescriptor) -> Result<NodeInfo, FileSystemError> {
        self.serve(|service| service.stat(delivery, fd))
    }

    fn rename(&self, delivery: &Delivery, from: &str, to: &str) -> Result<u32, FileSystemError> {
        self.serve(|service| service.rename(delivery, from, to))
    }

    fn unlink(
        &self,
        delivery: &Delivery,
        path: &str,
        expected: NodeKind,
    ) -> Result<u32, FileSystemError> {
        self.serve(|service| service.unlink(delivery, path, expected))
    }

    fn create_backed(
        &self,
        delivery: &Delivery,
        path: &str,
        region: IssuedHandle<kind::MemoryRegion>,
        size: u64,
    ) -> Result<u32, FileSystemError> {
        self.serve(|service| service.create_backed(delivery, path, region, size))
    }

    async fn backing_memory(
        &self,
        delivery: &Delivery,
        fd: NodeDescriptor,
        reply: file_system::backing_memory::Reply<Debt>,
    ) {
        let backing = async {
            let lease =
                Lease::descriptor(self.shared.clone(), self.volume, client_of(delivery)?, fd)?;
            let _locked = lease.lock.lock().await;
            self.serve(|service| service.backing_memory(delivery, fd))
        }
        .await;
        let node = backing.as_ref().ok().map(|backing| backing.node);

        if reply.send(backing.map(|backing| backing.region)).is_ok()
            && let Some(node) = node
        {
            let _ = self.shared.with_state(self.volume, |state| {
                let pinned = state.backend.pin(node);
                debug_assert!(pinned.is_ok());
                Ok(())
            });
        }
    }

    fn open_root(
        &self,
        delivery: &Delivery,
        path: &str,
    ) -> impl IntoWireValue<
        Delivery,
        Read = Result<ipc::wire::Session<file_system::Protocol>, FileSystemError>,
    > {
        self.serve(|service| service.open_root(delivery, path))
    }
}
