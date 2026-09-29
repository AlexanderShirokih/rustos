//! Сессии `Platform` и линии и регионы, которые они взяли.

use alloc::vec::Vec;

use bootstrap_abi::hardware;
use device_resolver::{
    DeviceSelector, DeviceTreePlatform, InterruptLine, PlatformDescription, ResolveError,
};
use ipc::{
    IntoWireValue,
    wire::{Cap, Str},
};
use kernel_objects::{
    Delivery, Error, IrqControl, IrqLine, MemoryRegion, ObjectKind, OwnedHandle, SessionTransport,
    kind,
};
use platform_abi::{MmioWindow, PlatformError, platform};
use runtime::{Mutex, log_fmt};
use syscall::{Rights, SyscallError};

pub(crate) struct SessionRecord {
    badge: u64,
    lines: Vec<HeldLine>,
    regions: Vec<HeldRegion>,
}

struct HeldLine {
    line: InterruptLine,
    root: IrqLine,
}

struct HeldRegion {
    base: u64,
    root: MemoryRegion,
}

pub(crate) struct Platform {
    sessions: &'static Mutex<Vec<SessionRecord>>,
    description: DeviceTreePlatform<'static>,
    lines: IrqControl,
    hardware: hardware::Client<SessionTransport>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Refusal {
    NoSession,
    Resolve(ResolveError),
    LineTaken,
    WindowTaken,
    WindowNotGranted,
    Mint,
    Copy,
}

impl SessionRecord {
    pub(crate) const fn new(badge: u64) -> Self {
        Self {
            badge,
            lines: Vec::new(),
            regions: Vec::new(),
        }
    }
}

impl Platform {
    pub(crate) const fn new(
        sessions: &'static Mutex<Vec<SessionRecord>>,
        description: DeviceTreePlatform<'static>,
        lines: IrqControl,
        hardware: hardware::Client<SessionTransport>,
    ) -> Self {
        Self {
            sessions,
            description,
            lines,
            hardware,
        }
    }

    pub(crate) fn close(&self, badge: u64) {
        // Удаление записи закрывает корни сессии, и ядро отзывает их копии у держателей.
        self.sessions.lock().retain(|record| record.badge != badge);
    }

    fn line(
        &self,
        badge: u64,
        compatible: &str,
        instance: u16,
        index: u16,
    ) -> Result<IrqLine, Refusal> {
        let mut sessions = self.sessions.lock();
        let record = sessions
            .iter_mut()
            .find(|record| record.badge == badge)
            .ok_or(Refusal::NoSession)?;
        let selector = DeviceSelector {
            compatible,
            instance,
        };
        let interrupt = self
            .description
            .interrupt(selector, index)
            .map_err(Refusal::Resolve)?;
        if let Some(held) = record.lines.iter().find(|held| held.line == interrupt.line) {
            return copy(&held.root);
        }
        // На линию, которую держит другая сессия, `IrqMint` отвечает `AlreadyBound`.
        let root = self
            .lines
            .mint(interrupt.line.number(), interrupt.trigger)
            .map_err(|error| match error {
                Error::Syscall(SyscallError::AlreadyBound) => Refusal::LineTaken,
                _ => Refusal::Mint,
            })?;
        let granted = copy(&root)?;
        record.lines.push(HeldLine {
            line: interrupt.line,
            root,
        });
        Ok(granted)
    }

    fn window(
        &self,
        badge: u64,
        compatible: &str,
        instance: u16,
        index: u16,
    ) -> Result<MmioWindow<OwnedHandle<kind::MemoryRegion>>, Refusal> {
        let mut sessions = self.sessions.lock();
        if !sessions.iter().any(|record| record.badge == badge) {
            return Err(Refusal::NoSession);
        }
        let selector = DeviceSelector {
            compatible,
            instance,
        };
        let mmio = self
            .description
            .mmio(selector, index)
            .map_err(Refusal::Resolve)?;
        let found = self
            .hardware
            .acquire_device_memory_containing::<MemoryRegion>(mmio.base, mmio.size)
            .ok()
            .flatten()
            .ok_or(Refusal::WindowNotGranted)?;
        let base = mmio.base - found.offset;
        // Регионы набора ядра не пересекаются, и совпадение базы означает общий регион.
        if sessions.iter().any(|record| {
            record.badge != badge && record.regions.iter().any(|held| held.base == base)
        }) {
            return Err(Refusal::WindowTaken);
        }
        let record = sessions
            .iter_mut()
            .find(|record| record.badge == badge)
            .ok_or(Refusal::NoSession)?;
        if !record.regions.iter().any(|held| held.base == base) {
            record.regions.push(HeldRegion {
                base,
                root: found.window,
            });
        }
        let held = record
            .regions
            .iter()
            .find(|held| held.base == base)
            .ok_or(Refusal::Copy)?;
        Ok(MmioWindow {
            region: copy(&held.root)?,
            offset: found.offset,
        })
    }

    fn refuse(
        compatible: &str,
        operation: &str,
        instance: u16,
        index: u16,
        refusal: Refusal,
    ) -> PlatformError {
        log_fmt(format_args!(
            "{compatible} {instance} {operation} {index} refused: {}",
            refusal.reason()
        ));
        refusal.error()
    }
}

impl platform::Service<Delivery> for Platform {
    fn irq(
        &mut self,
        frame: &Delivery,
        compatible: Str<'_, 255>,
        instance: u16,
        index: u16,
    ) -> impl IntoWireValue<Delivery, Read = Result<Cap<kind::IrqLine>, PlatformError>> {
        self.line(frame.badge, compatible.as_str(), instance, index)
            .map_err(|refusal| Self::refuse(compatible.as_str(), "irq", instance, index, refusal))
    }

    fn mmio(
        &mut self,
        frame: &Delivery,
        compatible: Str<'_, 255>,
        instance: u16,
        index: u16,
    ) -> impl IntoWireValue<Delivery, Read = Result<MmioWindow, PlatformError>> {
        self.window(frame.badge, compatible.as_str(), instance, index)
            .map_err(|refusal| Self::refuse(compatible.as_str(), "mmio", instance, index, refusal))
    }
}

impl Refusal {
    const fn error(self) -> PlatformError {
        match self {
            Self::Resolve(
                ResolveError::DeviceNotFound
                | ResolveError::NoInterrupts
                | ResolveError::NoMmio
                | ResolveError::ResourceOutOfRange,
            ) => PlatformError::NotDescribed,
            Self::LineTaken | Self::WindowTaken => PlatformError::Taken,
            Self::NoSession
            | Self::Resolve(
                ResolveError::UnknownController
                | ResolveError::Untranslatable
                | ResolveError::Malformed,
            )
            | Self::WindowNotGranted
            | Self::Mint
            | Self::Copy => PlatformError::Unavailable,
        }
    }

    const fn reason(self) -> &'static str {
        match self {
            Self::NoSession => "session not opened",
            Self::Resolve(ResolveError::DeviceNotFound) => "device not described by the platform",
            Self::Resolve(ResolveError::NoInterrupts) => "device described without interrupts",
            Self::Resolve(ResolveError::NoMmio) => "device described without mmio windows",
            Self::Resolve(ResolveError::ResourceOutOfRange) => "device resource out of range",
            Self::Resolve(ResolveError::UnknownController) => "interrupt controller unknown",
            Self::Resolve(ResolveError::Untranslatable) => "device address not translatable",
            Self::Resolve(ResolveError::Malformed) => "device description malformed",
            Self::LineTaken => "irq line held by another session",
            Self::WindowTaken => "mmio region held by another session",
            Self::WindowNotGranted => "mmio window not granted by the kernel",
            Self::Mint => "irq line not minted",
            Self::Copy => "copy not created",
        }
    }
}

fn copy<K: ObjectKind + 'static>(root: &OwnedHandle<K>) -> Result<OwnedHandle<K>, Refusal> {
    root.duplicate(
        Rights::READ | Rights::WRITE | Rights::DUPLICATE | Rights::TRANSFER,
        None,
    )
    .map_err(|_| Refusal::Copy)
}
