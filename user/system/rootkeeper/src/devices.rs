//! Корни линий прерываний и MMIO-окон устройств, которые rootkeeper держит за записями плана.

use alloc::{string::String, vec::Vec};

use bootstrap_abi::devices;
use component_abi::{MmioWindow, ResourceError};
use device_resolver::{
    DeviceSelector, DeviceTreePlatform, InterruptLine, PlatformDescription, ResolveError,
};
use device_resolver_aarch64::PLATFORM_BINDINGS;
use fdt::devicetree::DeviceTree;
use kernel_objects::{
    BorrowedHandle, IrqControl, IrqLine, MemoryRegion, ObjectKind, OwnedHandle, ReadOnlyMapping,
    Session, kind,
};
use runtime::PortTransport;
use userland_image::{BootPlan, PlanDevice, RequirementSource};

pub(crate) struct DeviceGrants {
    session: Session<devices::Protocol>,
    platform: Option<Platform>,
    lines: Vec<Root<InterruptLine, IrqLine>>,
    windows: Vec<Root<HeldWindow, MemoryRegion>>,
}

struct Platform {
    mapping: ReadOnlyMapping,
    offset: usize,
    _region: MemoryRegion,
    lines: IrqControl,
}

struct Root<D, H> {
    record: u16,
    requirement: String,
    device: D,
    handle: H,
}

#[derive(Clone, Copy)]
struct HeldWindow {
    region_base: u64,
    offset: u64,
}

pub(crate) trait DeviceKind: ObjectKind + Sized + 'static {
    type Grant;

    fn device(source: RequirementSource<'_>) -> Option<PlanDevice<'_>>;

    fn root(
        grants: &mut DeviceGrants,
        record: u16,
        requirement: &str,
        device: PlanDevice<'_>,
        grant: impl FnOnce(BorrowedHandle<'_, Self>) -> Result<OwnedHandle<Self>, ResourceError>,
    ) -> Result<Result<Self::Grant, ResourceError>, DeviceRefusal>;
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum DeviceRefusal {
    Platform,
    Resolve(ResolveError),
    LineTaken,
    WindowTaken,
    WindowNotGranted,
    Mint,
}

impl DeviceGrants {
    pub(crate) fn new(session: Session<devices::Protocol>, plan: &BootPlan<'_>) -> Self {
        let devices = plan.components().any(|component| {
            component.requirements().any(|requirement| {
                matches!(
                    requirement.source,
                    RequirementSource::Irq(_) | RequirementSource::Mmio(_)
                )
            })
        });
        let platform = devices.then(|| Platform::acquire(&session)).flatten();
        Self {
            session,
            platform,
            lines: Vec::new(),
            windows: Vec::new(),
        }
    }

    pub(crate) fn line<R>(
        &mut self,
        record: u16,
        requirement: &str,
        device: PlanDevice<'_>,
        grant: impl FnOnce(BorrowedHandle<'_, kind::IrqLine>) -> R,
    ) -> Result<R, DeviceRefusal> {
        if let Some(root) = self.lines.iter().find(|root| root.is(record, requirement)) {
            return Ok(grant(root.handle.borrow()));
        }
        let platform = self.platform.as_ref().ok_or(DeviceRefusal::Platform)?;
        let interrupt = platform.first_match(device, |description, selector| {
            description.interrupt(selector, device.resource)
        })?;
        if self.lines.iter().any(|root| root.device == interrupt.line) {
            return Err(DeviceRefusal::LineTaken);
        }
        let handle = platform
            .lines
            .mint(interrupt.line.number(), interrupt.trigger)
            .map_err(|_| DeviceRefusal::Mint)?;
        let answer = grant(handle.borrow());
        self.lines.push(Root {
            record,
            requirement: String::from(requirement),
            device: interrupt.line,
            handle,
        });
        Ok(answer)
    }

    pub(crate) fn window<R>(
        &mut self,
        record: u16,
        requirement: &str,
        device: PlanDevice<'_>,
        grant: impl FnOnce(BorrowedHandle<'_, kind::MemoryRegion>) -> R,
    ) -> Result<(R, u64), DeviceRefusal> {
        if let Some(root) = self
            .windows
            .iter()
            .find(|root| root.is(record, requirement))
        {
            return Ok((grant(root.handle.borrow()), root.device.offset));
        }
        let platform = self.platform.as_ref().ok_or(DeviceRefusal::Platform)?;
        let window = platform.first_match(device, |description, selector| {
            description.mmio(selector, device.resource)
        })?;
        let found = client(&self.session)
            .acquire_device_memory_containing::<MemoryRegion>(window.base, window.size)
            .ok()
            .flatten()
            .ok_or(DeviceRefusal::WindowNotGranted)?;
        let held = HeldWindow {
            region_base: window.base - found.offset,
            offset: found.offset,
        };
        // Регионы набора ядра не пересекаются, и совпадение базы означает общий регион.
        if self
            .windows
            .iter()
            .any(|root| root.device.region_base == held.region_base && root.record != record)
        {
            return Err(DeviceRefusal::WindowTaken);
        }
        let answer = grant(found.window.borrow());
        self.windows.push(Root {
            record,
            requirement: String::from(requirement),
            device: held,
            handle: found.window,
        });
        Ok((answer, held.offset))
    }
}

impl DeviceKind for kind::IrqLine {
    type Grant = OwnedHandle<Self>;

    fn device(source: RequirementSource<'_>) -> Option<PlanDevice<'_>> {
        match source {
            RequirementSource::Irq(device) => Some(device),
            _ => None,
        }
    }

    fn root(
        grants: &mut DeviceGrants,
        record: u16,
        requirement: &str,
        device: PlanDevice<'_>,
        grant: impl FnOnce(BorrowedHandle<'_, Self>) -> Result<OwnedHandle<Self>, ResourceError>,
    ) -> Result<Result<Self::Grant, ResourceError>, DeviceRefusal> {
        grants.line(record, requirement, device, grant)
    }
}

impl DeviceKind for kind::MemoryRegion {
    type Grant = MmioWindow<OwnedHandle<Self>>;

    fn device(source: RequirementSource<'_>) -> Option<PlanDevice<'_>> {
        match source {
            RequirementSource::Mmio(device) => Some(device),
            _ => None,
        }
    }

    fn root(
        grants: &mut DeviceGrants,
        record: u16,
        requirement: &str,
        device: PlanDevice<'_>,
        grant: impl FnOnce(BorrowedHandle<'_, Self>) -> Result<OwnedHandle<Self>, ResourceError>,
    ) -> Result<Result<Self::Grant, ResourceError>, DeviceRefusal> {
        let (granted, offset) = grants.window(record, requirement, device, grant)?;
        Ok(granted.map(|region| MmioWindow { region, offset }))
    }
}

impl<D, H> Root<D, H> {
    fn is(&self, record: u16, requirement: &str) -> bool {
        self.record == record && self.requirement == requirement
    }
}

impl DeviceRefusal {
    pub(crate) const fn reason(self) -> &'static str {
        match self {
            Self::Platform => "platform description unavailable",
            Self::Resolve(ResolveError::DeviceNotFound) => "device not described by the platform",
            Self::Resolve(ResolveError::NoInterrupts) => "device described without interrupts",
            Self::Resolve(ResolveError::NoMmio) => "device described without mmio windows",
            Self::Resolve(ResolveError::ResourceOutOfRange) => "device resource out of range",
            Self::Resolve(ResolveError::UnknownController) => "interrupt controller unknown",
            Self::Resolve(ResolveError::Untranslatable) => "device address not translatable",
            Self::Resolve(ResolveError::Malformed) => "device description malformed",
            Self::LineTaken => "irq line held by another requirement",
            Self::WindowTaken => "mmio region held by another record",
            Self::WindowNotGranted => "mmio window not granted by the kernel",
            Self::Mint => "irq line not minted",
        }
    }
}

impl Platform {
    fn acquire(session: &Session<devices::Protocol>) -> Option<Self> {
        let client = client(session);
        let fdt = client.acquire_boot_fdt().ok()?.ok()?;
        let region = MemoryRegion::from(fdt.region);
        let mapping = region.map_full_read_only().ok()?;
        let lines = IrqControl::from(client.acquire_irq_control().ok()?.ok()?);
        Some(Self {
            mapping,
            offset: usize::try_from(fdt.offset).ok()?,
            _region: region,
            lines,
        })
    }

    fn first_match<R>(
        &self,
        device: PlanDevice<'_>,
        resolve: impl Fn(&DeviceTreePlatform<'_>, DeviceSelector<'_>) -> Result<R, ResolveError>,
    ) -> Result<R, DeviceRefusal> {
        let blob = self
            .mapping
            .as_bytes()
            .get(self.offset..)
            .ok_or(DeviceRefusal::Platform)?;
        let tree = DeviceTree::from_bytes(blob).map_err(|_| DeviceRefusal::Platform)?;
        let description = DeviceTreePlatform::new(&tree, PLATFORM_BINDINGS);
        // Драйвер объявляет несколько строк совместимости, и описание этой машины содержит одну из
        // них. Отказ возвращается по последней строке.
        let mut failure = ResolveError::DeviceNotFound;
        for compatible in device.compatible() {
            let selector = DeviceSelector {
                compatible,
                instance: device.instance,
            };
            match resolve(&description, selector) {
                Ok(resolved) => return Ok(resolved),
                Err(error) => failure = error,
            }
        }
        Err(DeviceRefusal::Resolve(failure))
    }
}

fn client(session: &Session<devices::Protocol>) -> devices::Client<PortTransport> {
    devices::Client::new(PortTransport::new(session.handle().as_raw()))
}
