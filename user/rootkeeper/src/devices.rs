//! Корни линий прерываний и MMIO-окон устройств, которые rootkeeper держит за записями плана.

use alloc::{string::String, vec::Vec};

use bootstrap_abi::bootstrap;
use device_resolver::{
    DeviceMmio, DeviceSelector, DeviceTreePlatform, InterruptLine, PlatformDescription,
    ResolveError, acquire_mmio,
};
use device_resolver_aarch64::PLATFORM_BINDINGS;
use fdt::devicetree::DeviceTree;
use kernel_objects::{
    BorrowedHandle, IrqControl, IrqLine, MemoryRegion, ObjectKind, ReadOnlyMapping, kind,
};
use runtime::PortTransport;
use syscall::Handle;
use userland_image::{BootPlan, PlanDevice, RequirementSource};

pub(crate) struct DeviceGrants {
    kernel: Handle<kind::Port>,
    platform: Option<Platform>,
    lines: Vec<Root<InterruptLine, IrqLine>>,
    windows: Vec<Root<DeviceMmio, MemoryRegion>>,
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

pub(crate) trait DeviceKind: ObjectKind + Sized + 'static {
    fn device(source: RequirementSource<'_>) -> Option<PlanDevice<'_>>;

    fn root<R>(
        grants: &mut DeviceGrants,
        record: u16,
        requirement: &str,
        device: PlanDevice<'_>,
        grant: impl FnOnce(BorrowedHandle<'_, Self>) -> R,
    ) -> Result<R, DeviceRefusal>;
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum DeviceRefusal {
    Platform,
    Resolve(ResolveError),
    LineTaken,
    WindowTaken,
    Mint,
}

impl DeviceGrants {
    pub(crate) fn new(kernel: Handle<kind::Port>, plan: &BootPlan<'_>) -> Self {
        let devices = plan.components().any(|component| {
            component.requirements().any(|requirement| {
                matches!(
                    requirement.source,
                    RequirementSource::Irq(_) | RequirementSource::Mmio(_)
                )
            })
        });
        Self {
            kernel,
            platform: devices.then(|| Platform::acquire(kernel)).flatten(),
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
    ) -> Result<R, DeviceRefusal> {
        if let Some(root) = self
            .windows
            .iter()
            .find(|root| root.is(record, requirement))
        {
            return Ok(grant(root.handle.borrow()));
        }
        let platform = self.platform.as_ref().ok_or(DeviceRefusal::Platform)?;
        let window = platform.first_match(device, |description, selector| {
            description.mmio(selector, device.resource)
        })?;
        if self
            .windows
            .iter()
            .any(|root| overlaps(root.device, window))
        {
            return Err(DeviceRefusal::WindowTaken);
        }
        let client = bootstrap::Client::new(PortTransport::new(self.kernel));
        let handle: MemoryRegion = acquire_mmio(&client, window).map_err(DeviceRefusal::Resolve)?;
        let answer = grant(handle.borrow());
        self.windows.push(Root {
            record,
            requirement: String::from(requirement),
            device: window,
            handle,
        });
        Ok(answer)
    }
}

impl DeviceKind for kind::IrqLine {
    fn device(source: RequirementSource<'_>) -> Option<PlanDevice<'_>> {
        match source {
            RequirementSource::Irq(device) => Some(device),
            _ => None,
        }
    }

    fn root<R>(
        grants: &mut DeviceGrants,
        record: u16,
        requirement: &str,
        device: PlanDevice<'_>,
        grant: impl FnOnce(BorrowedHandle<'_, Self>) -> R,
    ) -> Result<R, DeviceRefusal> {
        grants.line(record, requirement, device, grant)
    }
}

impl DeviceKind for kind::MemoryRegion {
    fn device(source: RequirementSource<'_>) -> Option<PlanDevice<'_>> {
        match source {
            RequirementSource::Mmio(device) => Some(device),
            _ => None,
        }
    }

    fn root<R>(
        grants: &mut DeviceGrants,
        record: u16,
        requirement: &str,
        device: PlanDevice<'_>,
        grant: impl FnOnce(BorrowedHandle<'_, Self>) -> R,
    ) -> Result<R, DeviceRefusal> {
        grants.window(record, requirement, device, grant)
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
            Self::Resolve(ResolveError::BusTranslation) => "device behind a translating bus",
            Self::Resolve(ResolveError::Malformed) => "device description malformed",
            Self::Resolve(ResolveError::MmioUnavailable) => "mmio window not granted by the kernel",
            Self::LineTaken => "irq line held by another requirement",
            Self::WindowTaken => "mmio window overlaps another requirement",
            Self::Mint => "irq line not minted",
        }
    }
}

impl Platform {
    fn acquire(kernel: Handle<kind::Port>) -> Option<Self> {
        let client = bootstrap::Client::new(PortTransport::new(kernel));
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

fn overlaps(held: DeviceMmio, window: DeviceMmio) -> bool {
    let held_end = held.base.saturating_add(held.size);
    let window_end = window.base.saturating_add(window.size);
    held.base < window_end && window.base < held_end
}
