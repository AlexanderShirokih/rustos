//! Окна регистров арбитра, отображённые в процесс шины.

use kernel_objects::{MemoryRegion, MmioMapping, MmioWindow, SessionTransport};
use platform_abi::{DeviceId, PlatformError, platform};
use spmi_pmic_arb::{ArbiterNode, Registers, Window};

pub(crate) struct Windows {
    core: Region,
    channels: Region,
    observer: Region,
    interrupts: Region,
    config: Region,
}

struct Region {
    mapping: MmioMapping,
    size: usize,
    _window: MmioWindow,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Refusal {
    Call,
    Platform(PlatformError),
    Map,
}

impl Windows {
    pub(crate) fn take(
        platform: &platform::Client<SessionTransport>,
        id: &DeviceId,
        node: &ArbiterNode,
    ) -> Result<Self, Refusal> {
        let region = |window| Region::take(platform, id, node.resource(window));

        Ok(Self {
            core: region(Window::Core)?,
            channels: region(Window::Channels)?,
            observer: region(Window::Observer)?,
            interrupts: region(Window::Interrupts)?,
            config: region(Window::Config)?,
        })
    }

    fn region(&self, window: Window) -> &Region {
        match window {
            Window::Core => &self.core,
            Window::Channels => &self.channels,
            Window::Observer => &self.observer,
            Window::Interrupts => &self.interrupts,
            Window::Config => &self.config,
        }
    }
}

impl Region {
    fn take(
        platform: &platform::Client<SessionTransport>,
        id: &DeviceId,
        resource: u16,
    ) -> Result<Self, Refusal> {
        let window = platform
            .mmio(id.as_wire(), resource)
            .map_err(|error| error.fold(|_| Refusal::Call, Refusal::Platform))?;

        let window = MmioWindow {
            region: MemoryRegion::from(window.region),
            offset: window.offset,
            size: window.size,
        };

        let mapping = window.map().map_err(|_| Refusal::Map)?;
        let size = usize::try_from(window.size).map_err(|_| Refusal::Map)?;

        // Закрытие окна отзывает выведенные из него страницы, и `Region` хранит окно до сброса
        // отображения.
        Ok(Self {
            mapping,
            size,
            _window: window,
        })
    }
}

impl Registers for Windows {
    fn size(&self, window: Window) -> usize {
        self.region(window).size
    }

    fn load(&self, window: Window, offset: usize) -> u32 {
        self.region(window).mapping.mmio().read::<u32>(offset)
    }

    fn store(&self, window: Window, offset: usize, value: u32) {
        self.region(window)
            .mapping
            .mmio()
            .write::<u32>(offset, value);
    }
}
