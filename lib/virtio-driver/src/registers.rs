//! Доступ к регистрам и конфигурации транспорта запросами сессии слота шины.

use core::sync::atomic::{Ordering, fence};

use executor::PortChannel;
use ipc::{CallError, wire::IpcError};
use kernel_objects::SessionTransport;
use virtio::{AccessWidth, Registers};
use virtio_mmio_abi::{self as abi, SlotError, virtio_slot};

/// Отказ обращения к регистру или полю конфигурации через слот.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SlotFailure {
    /// Запрос к шине не доставлен либо ответ не получен.
    Ipc(IpcError),
    /// Шина отказала в обращении.
    Slot(SlotError),
    /// Смещение не помещается в поле запроса.
    Offset(usize),
}

/// Регистры транспорта, доступные через сессию слота.
pub struct SlotRegisters {
    slot: virtio_slot::Client<PortChannel<SessionTransport>>,
}

impl SlotRegisters {
    /// Обращается к регистрам транспорта слота `slot`.
    pub fn new(slot: virtio_slot::Client<PortChannel<SessionTransport>>) -> Self {
        Self { slot }
    }
}

impl Registers for SlotRegisters {
    type Error = SlotFailure;

    async fn read(&self, offset: usize) -> Result<u32, SlotFailure> {
        let offset = wire_offset(offset)?;
        self.slot.read(offset).await.map_err(failure)
    }

    async fn write(&mut self, offset: usize, value: u32) -> Result<(), SlotFailure> {
        let offset = wire_offset(offset)?;
        self.slot.write(offset, value).await.map_err(failure)
    }

    async fn read_config(&self, offset: usize, width: AccessWidth) -> Result<u32, SlotFailure> {
        let offset = wire_offset(offset)?;
        self.slot
            .config_read(offset, wire(width))
            .await
            .map_err(failure)
    }

    async fn write_config(
        &mut self,
        offset: usize,
        width: AccessWidth,
        value: u32,
    ) -> Result<(), SlotFailure> {
        let offset = wire_offset(offset)?;
        self.slot
            .config_write(offset, wire(width), value)
            .await
            .map_err(failure)
    }

    fn barrier(&mut self) {
        fence(Ordering::SeqCst);
    }
}

fn wire_offset(offset: usize) -> Result<u16, SlotFailure> {
    u16::try_from(offset).map_err(|_| SlotFailure::Offset(offset))
}

fn failure(error: CallError<SlotError>) -> SlotFailure {
    error.fold(SlotFailure::Ipc, SlotFailure::Slot)
}

fn wire(width: AccessWidth) -> abi::AccessWidth {
    match width {
        AccessWidth::Byte => abi::AccessWidth::Byte,
        AccessWidth::Half => abi::AccessWidth::Half,
        AccessWidth::Word => abi::AccessWidth::Word,
    }
}
