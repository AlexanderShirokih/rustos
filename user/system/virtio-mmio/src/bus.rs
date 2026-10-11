//! Транспорты шины, выдача слотов и обслуживание сессий на сервисном порту.
//!
//! Сессии `VirtioMmio` и сессии слотов выпускаются на одном порту из общего счётчика значков.
//! Кадр значка, которым занят транспорт, идёт протоколу `VirtioSlot`, остальные кадры -
//! протоколу `VirtioMmio`.

use alloc::vec::Vec;
use core::sync::atomic::{Ordering, fence};

use component_startup::{SessionEvent, SessionPort};
use ipc::{IntoWireValue, wire::Cap};
use kernel_objects::{Delivery, IrqLine, MmioMapping, Port, SessionTransport};
use platform_abi::{DeviceId, MappedDescription, PlatformError, WindowRefusal, platform};
use runtime::log_fmt;
use syscall::{Rights, kind};
use virtio::register;
use virtio_mmio_abi::{AccessWidth, SlotError, VirtioMmioError, virtio_mmio, virtio_slot};

const COMPATIBLE: &str = "virtio,mmio";

const REGISTER_BYTES: u64 = 4;
const CONFIG_OFFSET: u64 = 0x100;

const LINE_RIGHTS: Rights = Rights::from_bits_truncate(
    Rights::READ.bits() | Rights::WRITE.bits() | Rights::DUPLICATE.bits() | Rights::TRANSFER.bits(),
);

pub(crate) struct Bus {
    transports: Vec<Transport>,
}

struct Transport {
    id: DeviceId,
    device_type: u32,
    registers: MmioMapping,
    line: IrqLine,
    slot: Slot,
}

enum Slot {
    Free,
    Held { badge: u64, lease: Option<IrqLine> },
    Resetting,
}

struct Opener<'a> {
    sessions: &'a SessionPort,
    transports: &'a mut [Transport],
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Refusal {
    Call,
    Platform(PlatformError),
    Window(WindowRefusal),
}

impl Bus {
    pub(crate) fn take(platform: &platform::Client<SessionTransport>) -> Option<Self> {
        let mapped = MappedDescription::new(platform.description().ok()?).ok()?;
        let description = mapped.description().ok()?;

        let mut transports = Vec::new();
        for device in description.compatible_devices(COMPATIBLE) {
            match Transport::take(platform, device.id()) {
                Ok(transport) => transports.push(transport),
                Err(refusal) => log_fmt(format_args!(
                    "{} skipped: {refusal:?}",
                    device.id().as_str()
                )),
            }
        }
        log_fmt(format_args!("{} transports taken", transports.len()));
        Some(Self { transports })
    }

    pub(crate) fn serve(&mut self, sessions: &SessionPort) {
        let mut events = sessions.events();

        while let Some(event) = events.recv() {
            match event {
                SessionEvent::Frame(frame) => {
                    let badge = frame.resources.badge;
                    if let Some(slot) = self.slot(badge) {
                        let _ = virtio_slot::dispatch(slot, frame);
                    } else {
                        let mut opener = Opener {
                            sessions,
                            transports: &mut self.transports,
                        };
                        let _ = virtio_mmio::dispatch(&mut opener, frame);
                    }
                }
                SessionEvent::Gone(badge) => {
                    if let Some(slot) = self.slot(badge) {
                        slot.release();
                    }
                }
            }
        }
    }

    fn slot(&mut self, badge: u64) -> Option<&mut Transport> {
        self.transports.iter_mut().find(
            |transport| matches!(transport.slot, Slot::Held { badge: held, .. } if held == badge),
        )
    }
}

impl Transport {
    fn take(platform: &platform::Client<SessionTransport>, id: &DeviceId) -> Result<Self, Refusal> {
        let line = platform
            .irq(id.as_str(), 0)
            .map_err(|error| error.fold(|_| Refusal::Call, Refusal::Platform))?;
        let registers = platform.map_mmio(id.as_str(), 0).map_err(Refusal::Window)?;
        let device_type = registers.mmio().read::<u32>(register::DEVICE_ID);

        Ok(Self {
            id: id.clone(),
            device_type,
            registers,
            line: IrqLine::from(line),
            slot: Slot::Free,
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

    fn release(&mut self) {
        self.slot = Slot::Resetting;
        self.registers.mmio().write::<u32>(register::STATUS, 0);

        self.settle();
        if matches!(self.slot, Slot::Resetting) {
            log_fmt(format_args!("{} reset pending", self.id.as_str()));
        }
    }

    fn settle(&mut self) {
        if matches!(self.slot, Slot::Resetting)
            && self.registers.mmio().read::<u32>(register::STATUS) == 0
        {
            self.slot = Slot::Free;
        }
    }
}

impl Opener<'_> {
    fn open(&mut self, device_type: u32) -> Result<Port, VirtioMmioError> {
        self.transports.iter_mut().for_each(Transport::settle);
        let mut matching = self
            .transports
            .iter_mut()
            .filter(|transport| device_type != 0 && transport.device_type == device_type)
            .peekable();
        if matching.peek().is_none() {
            return Err(VirtioMmioError::NoDevice);
        }

        let transport = matching
            .find(|transport| matches!(transport.slot, Slot::Free))
            .ok_or(VirtioMmioError::Busy)?;
        let minted = self
            .sessions
            .create_session()
            .map_err(|_| VirtioMmioError::NoSpace)?;
        transport.slot = Slot::Held {
            badge: minted.badge.get(),
            lease: None,
        };
        Ok(minted.port)
    }
}

impl virtio_mmio::Service<Delivery> for Opener<'_> {
    fn open(
        &mut self,
        _frame: &Delivery,
        device_type: u32,
    ) -> impl IntoWireValue<
        Delivery,
        Read = Result<ipc::wire::Session<virtio_slot::Protocol>, VirtioMmioError>,
    > {
        // Порт сессии слота выпускает эта шина, и она же обслуживает на нём `VirtioSlot`.
        Opener::open(self, device_type).map(kernel_objects::Session::from_handle)
    }
}

impl virtio_slot::Service<Delivery> for Transport {
    fn read(&mut self, _frame: &Delivery, offset: u16) -> Result<u32, SlotError> {
        let offset = self.register(offset)?;
        Ok(self.load(offset, AccessWidth::Word))
    }

    fn write(&mut self, _frame: &Delivery, offset: u16, value: u32) -> Result<(), SlotError> {
        let offset = self.register(offset)?;
        self.store(offset, AccessWidth::Word, value);
        Ok(())
    }

    fn config_read(
        &mut self,
        _frame: &Delivery,
        offset: u16,
        width: AccessWidth,
    ) -> Result<u32, SlotError> {
        let offset = self.config(offset, width)?;
        Ok(self.load(offset, width))
    }

    fn config_write(
        &mut self,
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
        &mut self,
        _frame: &Delivery,
    ) -> impl IntoWireValue<Delivery, Read = Result<Cap<kind::IrqLine>, SlotError>> {
        let Slot::Held { lease, .. } = &mut self.slot else {
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
