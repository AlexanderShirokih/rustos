//! Доступ к регистрам транспорта запросами сессии слота шины.

use core::sync::atomic::{Ordering, fence};

use ipc::wire::IpcError;
use kernel_objects::SessionTransport;
use virtio::Registers;
use virtio_mmio_abi::{SlotError, virtio_slot};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum SlotFailure {
    Ipc(IpcError),
    Slot(SlotError),
    Offset(usize),
}

pub(crate) struct RemoteRegisters {
    slot: virtio_slot::Client<SessionTransport>,
}

impl RemoteRegisters {
    pub(crate) fn new(slot: virtio_slot::Client<SessionTransport>) -> Self {
        Self { slot }
    }
}

impl Registers for RemoteRegisters {
    type Error = SlotFailure;

    fn read(&self, offset: usize) -> Result<u32, SlotFailure> {
        let offset = u16::try_from(offset).map_err(|_| SlotFailure::Offset(offset))?;
        self.slot
            .read(offset)
            .map_err(|error| error.fold(SlotFailure::Ipc, SlotFailure::Slot))
    }

    fn write(&mut self, offset: usize, value: u32) -> Result<(), SlotFailure> {
        let offset = u16::try_from(offset).map_err(|_| SlotFailure::Offset(offset))?;
        self.slot
            .write(offset, value)
            .map_err(|error| error.fold(SlotFailure::Ipc, SlotFailure::Slot))
    }

    fn barrier(&mut self) {
        fence(Ordering::SeqCst);
    }
}
