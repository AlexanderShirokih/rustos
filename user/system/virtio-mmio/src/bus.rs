//! Транспорты шины, выдача слотов и обслуживание сессий на сервисном порту.
//!
//! Сессии `VirtioMmio` и сессии слотов выпускаются на одном порту из общего счётчика значков.
//! Кадр значка, которым занят транспорт, идёт протоколу `VirtioSlot`, остальные кадры -
//! протоколу `VirtioMmio`.

use alloc::vec::Vec;
use core::sync::atomic::{AtomicU64, Ordering, fence};

use component_startup::mint_session;
use ipc::{
    IntoWireValue, Listener, RecvError,
    wire::{Cap, MESSAGE_INLINE_MAX},
};
use kernel_objects::{
    Delivery, IrqLine, MemoryRegion, MmioMapping, MmioWindow, Port, SessionTransport,
};
use platform_abi::{Description, DeviceId, PlatformError, platform};
use runtime::{PortTransport, Timeout, log_fmt};
use syscall::{Rights, kind};
use virtio::register;
use virtio_mmio_abi::{SlotError, VirtioMmioError, virtio_mmio, virtio_slot};

const COMPATIBLE: &str = "virtio,mmio";

const REGISTER_BYTES: u64 = 4;

const LINE_RIGHTS: Rights = Rights::from_bits_truncate(
    Rights::READ.bits() | Rights::WRITE.bits() | Rights::DUPLICATE.bits() | Rights::TRANSFER.bits(),
);

pub(crate) struct Sessions {
    pub(crate) service: Port,
    pub(crate) next_badge: AtomicU64,
}

pub(crate) struct Bus {
    transports: Vec<Transport>,
}

struct Transport {
    id: DeviceId,
    device_type: u32,
    registers: MmioMapping,
    window: MmioWindow,
    line: IrqLine,
    slot: Slot,
}

enum Slot {
    Free,
    Held { badge: u64, lease: Option<IrqLine> },
    Resetting,
}

struct Opener<'a> {
    sessions: &'a Sessions,
    transports: &'a mut [Transport],
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Refusal {
    Call,
    Platform(PlatformError),
    Map,
}

impl Bus {
    pub(crate) fn take(platform: &platform::Client<SessionTransport>) -> Option<Self> {
        let description = platform.description().ok()?.ok()?;
        // Регион описания нужен только на время обхода узлов.
        let region = MemoryRegion::from(description.region);
        let mapping = region.map_full_read_only().ok()?;
        let description = Description::new(mapping.as_bytes(), description.offset).ok()?;

        let mut transports = Vec::new();
        for device in description.devices().filter_map(Result::ok) {
            if !device
                .compatible()
                .any(|compatible| compatible == COMPATIBLE)
            {
                continue;
            }

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

    pub(crate) fn serve(&mut self, sessions: &Sessions) {
        let transport = PortTransport::new(sessions.service.borrow());
        let mut bytes = [0u8; MESSAGE_INLINE_MAX];
        // Отброшенный кадр порт не закрывает, а закрытие порта терминально.
        loop {
            match transport.recv(&mut bytes, Timeout::INFINITE) {
                Ok(frame) => {
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
                Err(RecvError::SenderGone(badge)) => {
                    if let Some(slot) = self.slot(badge) {
                        slot.release();
                    }
                }
                Err(RecvError::Discarded | RecvError::Timeout) => {}
                Err(RecvError::Closed) => break,
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
            .irq(id.as_wire(), 0)
            .map_err(|_| Refusal::Call)?
            .map_err(Refusal::Platform)?;
        let window = platform
            .mmio(id.as_wire(), 0)
            .map_err(|_| Refusal::Call)?
            .map_err(Refusal::Platform)?;
        let window = MmioWindow {
            region: MemoryRegion::from(window.region),
            offset: window.offset,
            size: window.size,
        };
        let registers = window.map().map_err(|_| Refusal::Map)?;
        let device_type = registers.mmio().read::<u32>(register::DEVICE_ID);

        // Транспорт держит окно до конца процесса, потому что закрытие окна отзывает выведенные
        // из него страницы.
        Ok(Self {
            id: id.clone(),
            device_type,
            registers,
            window,
            line: IrqLine::from(line),
            slot: Slot::Free,
        })
    }

    fn register(&self, offset: u16) -> Result<usize, SlotError> {
        let aligned = u64::from(offset) % REGISTER_BYTES == 0;
        let inside = u64::from(offset) + REGISTER_BYTES <= self.window.size;

        if aligned && inside {
            Ok(usize::from(offset))
        } else {
            Err(SlotError::OutOfRange)
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
        let minted = mint_session(&self.sessions.service, &self.sessions.next_badge)
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
        Ok(self.registers.mmio().read::<u32>(offset))
    }

    fn write(&mut self, _frame: &Delivery, offset: u16, value: u32) -> Result<(), SlotError> {
        let offset = self.register(offset)?;
        // Запись клиента в общую с устройством память завершена до его запроса.
        fence(Ordering::SeqCst);
        self.registers.mmio().write::<u32>(offset, value);
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
