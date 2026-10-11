//! Окна регистров арбитра, отображённые в процесс шины.

use kernel_objects::{MmioMapping, SessionTransport};
use platform_abi::{DeviceId, WindowRefusal, platform};
use spmi_pmic_arb::{ArbiterNode, Registers, Window};

pub(crate) struct Windows {
    core: MmioMapping,
    channels: MmioMapping,
    observer: MmioMapping,
    interrupts: MmioMapping,
    config: MmioMapping,
}

impl Windows {
    pub(crate) fn take(
        platform: &platform::Client<SessionTransport>,
        id: &DeviceId,
        node: &ArbiterNode,
    ) -> Result<Self, WindowRefusal> {
        let region = |window| platform.map_mmio(id.as_str(), node.resource(window));

        Ok(Self {
            core: region(Window::Core)?,
            channels: region(Window::Channels)?,
            observer: region(Window::Observer)?,
            interrupts: region(Window::Interrupts)?,
            config: region(Window::Config)?,
        })
    }

    fn region(&self, window: Window) -> &MmioMapping {
        match window {
            Window::Core => &self.core,
            Window::Channels => &self.channels,
            Window::Observer => &self.observer,
            Window::Interrupts => &self.interrupts,
            Window::Config => &self.config,
        }
    }
}

impl Registers for Windows {
    fn size(&self, window: Window) -> usize {
        self.region(window).size()
    }

    fn load(&self, window: Window, offset: usize) -> u32 {
        self.region(window).mmio().read::<u32>(offset)
    }

    fn store(&self, window: Window, offset: usize, value: u32) {
        self.region(window).mmio().write::<u32>(offset, value);
    }
}
