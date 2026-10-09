//! Сессии `Platform` и описание, линии и регионы, которые они взяли.

use alloc::vec::Vec;
use core::fmt;

use bootstrap_abi::hardware;
use device_resolver::{DeviceTreePlatform, InterruptLine, PlatformDescription, ResolveError};
use ipc::{IntoWireValue, wire::Cap};
use kernel_objects::{
    Delivery, Error, IrqControl, IrqLine, MemoryRegion, ObjectKind, OwnedHandle, SessionTransport,
    kind,
};
use platform_abi::{DescriptionRegion, DeviceId, MmioWindow, PlatformError, platform};
use runtime::{Mutex, log_fmt};
use syscall::{MemoryAccess, Rights, SyscallError};

const RESOURCE_RIGHTS: Rights = Rights::from_bits_truncate(
    Rights::READ.bits() | Rights::WRITE.bits() | Rights::DUPLICATE.bits() | Rights::TRANSFER.bits(),
);

const DESCRIPTION_RIGHTS: Rights = Rights::from_bits_truncate(
    Rights::READ.bits() | Rights::DUPLICATE.bits() | Rights::TRANSFER.bits(),
);

pub(crate) struct SessionRecord {
    badge: u64,
    description: Option<MemoryRegion>,
    lines: Vec<HeldLine>,
    regions: Vec<HeldRegion>,
}

pub(crate) struct HardwareDescription {
    pub(crate) region: &'static MemoryRegion,
    pub(crate) offset: u64,
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
    resolver: DeviceTreePlatform<'static>,
    description: HardwareDescription,
    lines: IrqControl,
    hardware: hardware::Client<SessionTransport>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Refusal {
    NoSession,
    InvalidDevice,
    Resolve(ResolveError),
    LineTaken,
    WindowTaken,
    WindowNotGranted,
    Mint,
    Slice,
    Copy,
}

impl SessionRecord {
    pub(crate) const fn new(badge: u64) -> Self {
        Self {
            badge,
            description: None,
            lines: Vec::new(),
            regions: Vec::new(),
        }
    }
}

impl Platform {
    pub(crate) const fn new(
        sessions: &'static Mutex<Vec<SessionRecord>>,
        resolver: DeviceTreePlatform<'static>,
        description: HardwareDescription,
        lines: IrqControl,
        hardware: hardware::Client<SessionTransport>,
    ) -> Self {
        Self {
            sessions,
            resolver,
            description,
            lines,
            hardware,
        }
    }

    pub(crate) fn close(&self, badge: u64) {
        // Удаление записи закрывает корни сессии, и ядро отзывает их копии у держателей.
        self.sessions.lock().retain(|record| record.badge != badge);
    }

    fn granted_description(
        &self,
        badge: u64,
    ) -> Result<DescriptionRegion<OwnedHandle<kind::MemoryRegion>>, Refusal> {
        let mut sessions = self.sessions.lock();
        let record = sessions
            .iter_mut()
            .find(|record| record.badge == badge)
            .ok_or(Refusal::NoSession)?;
        let root = match &mut record.description {
            Some(root) => root,
            empty @ None => {
                let size = self
                    .description
                    .region
                    .inspect()
                    .map_err(|_| Refusal::Slice)?
                    .size_bytes;
                let root = self
                    .description
                    .region
                    .slice(0, size, MemoryAccess::READ)
                    .map_err(|_| Refusal::Slice)?;
                empty.insert(root)
            }
        };
        Ok(DescriptionRegion {
            region: copy(root, DESCRIPTION_RIGHTS)?,
            offset: self.description.offset,
        })
    }

    fn line(&self, badge: u64, device: &str, resource: u16) -> Result<IrqLine, Refusal> {
        let mut sessions = self.sessions.lock();
        let record = sessions
            .iter_mut()
            .find(|record| record.badge == badge)
            .ok_or(Refusal::NoSession)?;
        let device = DeviceId::new(device).ok_or(Refusal::InvalidDevice)?;
        let interrupt = self
            .resolver
            .interrupt(&device, resource)
            .map_err(Refusal::Resolve)?;
        if let Some(held) = record.lines.iter().find(|held| held.line == interrupt.line) {
            return copy(&held.root, RESOURCE_RIGHTS);
        }
        // На линию, которую держит другая сессия, `IrqMint` отвечает `AlreadyBound`.
        let root = self
            .lines
            .mint(interrupt.line.number(), interrupt.trigger)
            .map_err(|error| match error {
                Error::Syscall(SyscallError::AlreadyBound) => Refusal::LineTaken,
                _ => Refusal::Mint,
            })?;
        let granted = copy(&root, RESOURCE_RIGHTS)?;
        record.lines.push(HeldLine {
            line: interrupt.line,
            root,
        });
        Ok(granted)
    }

    fn window(
        &self,
        badge: u64,
        device: &str,
        resource: u16,
    ) -> Result<MmioWindow<OwnedHandle<kind::MemoryRegion>>, Refusal> {
        let mut sessions = self.sessions.lock();
        if !sessions.iter().any(|record| record.badge == badge) {
            return Err(Refusal::NoSession);
        }
        let device = DeviceId::new(device).ok_or(Refusal::InvalidDevice)?;
        let mmio = self
            .resolver
            .mmio(&device, resource)
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
        let region = if let Some(held) = record.regions.iter().find(|held| held.base == base) {
            copy(&held.root, RESOURCE_RIGHTS)?
        } else {
            let region = copy(&found.window, RESOURCE_RIGHTS)?;
            record.regions.push(HeldRegion {
                base,
                root: found.window,
            });
            region
        };
        Ok(MmioWindow {
            region,
            offset: found.offset,
            size: mmio.size,
        })
    }

    fn refuse(request: fmt::Arguments<'_>, refusal: Refusal) -> PlatformError {
        log_fmt(format_args!("{request} refused: {}", refusal.reason()));
        refusal.error()
    }
}

impl platform::Service<Delivery> for Platform {
    fn irq(
        &mut self,
        frame: &Delivery,
        device: &str,
        resource: u16,
    ) -> impl IntoWireValue<Delivery, Read = Result<Cap<kind::IrqLine>, PlatformError>> {
        self.line(frame.badge, device, resource)
            .map_err(|refusal| Self::refuse(format_args!("{device} irq {resource}"), refusal))
    }

    fn mmio(
        &mut self,
        frame: &Delivery,
        device: &str,
        resource: u16,
    ) -> impl IntoWireValue<Delivery, Read = Result<MmioWindow, PlatformError>> {
        self.window(frame.badge, device, resource)
            .map_err(|refusal| Self::refuse(format_args!("{device} mmio {resource}"), refusal))
    }

    fn description(
        &mut self,
        frame: &Delivery,
    ) -> impl IntoWireValue<Delivery, Read = Result<DescriptionRegion, PlatformError>> {
        self.granted_description(frame.badge)
            .map_err(|refusal| Self::refuse(format_args!("description"), refusal))
    }
}

impl Refusal {
    const fn error(self) -> PlatformError {
        match self {
            Self::InvalidDevice
            | Self::Resolve(
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
            | Self::Slice
            | Self::Copy => PlatformError::Unavailable,
        }
    }

    const fn reason(self) -> &'static str {
        match self {
            Self::NoSession => "session not opened",
            Self::InvalidDevice => "device id malformed",
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
            Self::Slice => "description region not derived",
            Self::Copy => "copy not created",
        }
    }
}

fn copy<K: ObjectKind + 'static>(
    root: &OwnedHandle<K>,
    rights: Rights,
) -> Result<OwnedHandle<K>, Refusal> {
    root.duplicate(rights, None).map_err(|_| Refusal::Copy)
}
