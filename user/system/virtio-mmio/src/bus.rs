//! Транспорты шины, выдача слотов и обслуживание сессий на сервисном порту.
//!
//! Сессии `VirtioMmio` и сессии слотов выпускаются на одном порту из общего счётчика значков.
//! Кадр значка, которым занят транспорт, идёт протоколу `VirtioSlot`, остальные кадры -
//! протоколу `VirtioMmio`.

use alloc::{rc::Rc, vec::Vec};
use core::{
    cell::RefCell,
    sync::atomic::{Ordering, fence},
};

use component_startup::SessionPort;
use executor::PortChannel;
use ipc::{
    Incoming, IntoWireValue,
    wire::{Cap, IpcError},
};
use kernel_objects::{Delivery, IrqLine, MmioMapping, Port, SessionTransport};
use platform_abi::{DeviceId, MappedDescription, PlatformError, WindowRefusal, platform};
use runtime::log_fmt;
use syscall::{Rights, kind};
use virtio::register;
use virtio_mmio_abi::{AccessWidth, SlotError, VirtioMmioError, virtio_mmio, virtio_slot};

const COMPATIBLE: &str = "virtio,mmio";

const REGISTER_BYTES: u64 = 4;
const CONFIG_OFFSET: u64 = 0x100;

const LINE_RIGHTS: Rights = Rights::READ
    .union(Rights::WRITE)
    .union(Rights::DUPLICATE)
    .union(Rights::TRANSFER);

pub(crate) struct Bus {
    transports: Vec<Transport>,
    sessions: Rc<SessionPort>,
}

struct Transport {
    id: DeviceId,
    device_type: u32,
    registers: MmioMapping,
    line: IrqLine,
    slot: RefCell<Slot>,
}

enum Slot {
    Free,
    Held { badge: u64, lease: Option<IrqLine> },
    Resetting,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Refusal {
    Call,
    Platform(PlatformError),
    Window(WindowRefusal),
}

impl Bus {
    pub(crate) async fn take(
        platform: &platform::Client<PortChannel<SessionTransport>>,
        sessions: Rc<SessionPort>,
    ) -> Option<Self> {
        let region = platform.description().await.ok()?;
        let mapped = MappedDescription::new(region).ok()?;
        let description = mapped.description().ok()?;

        let mut transports = Vec::new();
        for device in description.compatible_devices(COMPATIBLE) {
            match Transport::take(platform, device.id()).await {
                Ok(transport) => transports.push(transport),
                Err(refusal) => log_fmt(format_args!(
                    "{} skipped: {refusal:?}",
                    device.id().as_str()
                )),
            }
        }

        log_fmt(format_args!("{} transports taken", transports.len()));
        Some(Self {
            transports,
            sessions,
        })
    }

    pub(crate) fn dispatch(&self, frame: Incoming<'_, Delivery>) -> Result<(), IpcError> {
        if let Some(slot) = self.slot(frame.resources.badge) {
            virtio_slot::dispatch(slot, frame)
        } else {
            virtio_mmio::dispatch(self, frame)
        }
    }

    pub(crate) fn gone(&self, badge: u64) {
        if let Some(slot) = self.slot(badge) {
            slot.release();
        }
    }

    fn slot(&self, badge: u64) -> Option<&Transport> {
        self.transports.iter().find(
            |transport| matches!(*transport.slot.borrow(), Slot::Held { badge: held, .. } if held == badge),
        )
    }
}

impl Transport {
    async fn take(
        platform: &platform::Client<PortChannel<SessionTransport>>,
        id: &DeviceId,
    ) -> Result<Self, Refusal> {
        let line = platform
            .irq(id.clone(), 0)
            .await
            .map_err(|error| error.fold(|_| Refusal::Call, Refusal::Platform))?;

        let registers = platform.map_mmio(id, 0).await.map_err(Refusal::Window)?;
        let device_type = registers.mmio().read::<u32>(register::DEVICE_ID);

        Ok(Self {
            id: id.clone(),
            device_type,
            registers,
            line: IrqLine::from(line),
            slot: RefCell::new(Slot::Free),
        })
    }

    fn register(&self, offset: u16) -> Result<usize, SlotError> {
        self.access(u64::from(offset), REGISTER_BYTES)
    }

    fn config(&self, offset: u16, width: AccessWidth) -> Result<usize, SlotError> {
        self.access(
            CONFIG_OFFSET + u64::from(offset),
            u64::from(u8::from(width)),
        )
    }

    fn load(&self, offset: usize, width: AccessWidth) -> u32 {
        let mmio = self.registers.mmio();
        match width {
            AccessWidth::Byte => u32::from(mmio.read::<u8>(offset)),
            AccessWidth::Half => u32::from(mmio.read::<u16>(offset)),
            AccessWidth::Word => mmio.read::<u32>(offset),
        }
    }

    fn store(&self, offset: usize, width: AccessWidth, value: u32) {
        let [b0, b1, ..] = value.to_le_bytes();
        // Запись клиента в общую с устройством память завершена до его запроса.
        fence(Ordering::SeqCst);
        let mmio = self.registers.mmio();
        match width {
            AccessWidth::Byte => mmio.write::<u8>(offset, b0),
            AccessWidth::Half => mmio.write::<u16>(offset, u16::from_le_bytes([b0, b1])),
            AccessWidth::Word => mmio.write::<u32>(offset, value),
        }
    }

    fn access(&self, offset: u64, bytes: u64) -> Result<usize, SlotError> {
        let aligned = offset.is_multiple_of(bytes);
        let inside = u64::try_from(self.registers.size()).is_ok_and(|size| offset + bytes <= size);

        match usize::try_from(offset) {
            Ok(offset) if aligned && inside => Ok(offset),
            _ => Err(SlotError::OutOfRange),
        }
    }

    fn release(&self) {
        *self.slot.borrow_mut() = Slot::Resetting;
        self.registers.mmio().write::<u32>(register::STATUS, 0);

        self.settle();
        if matches!(*self.slot.borrow(), Slot::Resetting) {
            log_fmt(format_args!("{} reset pending", self.id.as_str()));
        }
    }

    fn settle(&self) {
        if matches!(*self.slot.borrow(), Slot::Resetting)
            && self.registers.mmio().read::<u32>(register::STATUS) == 0
        {
            *self.slot.borrow_mut() = Slot::Free;
        }
    }
}

impl Bus {
    fn open(&self, device_type: u32) -> Result<Port, VirtioMmioError> {
        self.transports.iter().for_each(Transport::settle);
        let mut matching = self
            .transports
            .iter()
            .filter(|transport| device_type != 0 && transport.device_type == device_type)
            .peekable();
        if matching.peek().is_none() {
            return Err(VirtioMmioError::NoDevice);
        }

        let transport = matching
            .find(|transport| matches!(*transport.slot.borrow(), Slot::Free))
            .ok_or(VirtioMmioError::Busy)?;
        let minted = self
            .sessions
            .create_session()
            .map_err(|_| VirtioMmioError::NoSpace)?;
        *transport.slot.borrow_mut() = Slot::Held {
            badge: minted.badge.get(),
            lease: None,
        };

        Ok(minted.port)
    }
}

#[allow(clippy::unused_async_trait_impl)]
impl virtio_mmio::Service<Delivery> for Bus {
    fn open(
        &self,
        _frame: &Delivery,
        device_type: u32,
    ) -> impl IntoWireValue<
        Delivery,
        Read = Result<ipc::wire::Session<virtio_slot::Protocol>, VirtioMmioError>,
    > {
        // Порт сессии слота выпускает эта шина, и она же обслуживает на нём `VirtioSlot`.
        Bus::open(self, device_type).map(kernel_objects::Session::from_handle)
    }
}

#[allow(clippy::unused_async_trait_impl)]
impl virtio_slot::Service<Delivery> for Transport {
    fn read(&self, _frame: &Delivery, offset: u16) -> Result<u32, SlotError> {
        let offset = self.register(offset)?;
        Ok(self.load(offset, AccessWidth::Word))
    }

    fn write(&self, _frame: &Delivery, offset: u16, value: u32) -> Result<(), SlotError> {
        let offset = self.register(offset)?;
        self.store(offset, AccessWidth::Word, value);
        Ok(())
    }

    fn config_read(
        &self,
        _frame: &Delivery,
        offset: u16,
        width: AccessWidth,
    ) -> Result<u32, SlotError> {
        let offset = self.config(offset, width)?;
        Ok(self.load(offset, width))
    }

    fn config_write(
        &self,
        _frame: &Delivery,
        offset: u16,
        width: AccessWidth,
        value: u32,
    ) -> Result<(), SlotError> {
        let offset = self.config(offset, width)?;
        self.store(offset, width, value);
        Ok(())
    }

    fn irq(
        &self,
        _frame: &Delivery,
    ) -> impl IntoWireValue<Delivery, Read = Result<Cap<kind::IrqLine>, SlotError>> {
        let mut slot = self.slot.borrow_mut();
        let Slot::Held { lease, .. } = &mut *slot else {
            return Err(SlotError::Unavailable);
        };
        // Копии держателя выводятся из `lease`, и уход держателя отзывает их вместе с ним.
        if lease.is_none() {
            *lease = self.line.duplicate(LINE_RIGHTS, None).ok();
        }

        lease
            .as_ref()
            .ok_or(SlotError::Unavailable)?
            .duplicate(LINE_RIGHTS, None)
            .map_err(|_| SlotError::Unavailable)
    }
}
