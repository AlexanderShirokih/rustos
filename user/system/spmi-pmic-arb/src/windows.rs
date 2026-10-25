//! Окна регистров арбитра, отображённые в процесс шины.

use executor::PortChannel;
use kernel_objects::{MmioMapping, SessionTransport};
use mmio::MappedWindows;
use platform_abi::{DeviceId, WindowRefusal, platform};
use spmi_pmic_arb::{ArbiterNode, Window};

pub(crate) struct Windows {
    core: MmioMapping,
    channels: MmioMapping,
    observer: MmioMapping,
    interrupts: MmioMapping,
    config: MmioMapping,
}

impl Windows {
    pub(crate) async fn take(
        platform: &platform::Client<PortChannel<SessionTransport>>,
        id: &DeviceId,
        node: &ArbiterNode,
    ) -> Result<Self, WindowRefusal> {
        let region = async |window| platform.map_mmio(id, node.resource(window)).await;

        Ok(Self {
            core: region(Window::Core).await?,
            channels: region(Window::Channels).await?,
            observer: region(Window::Observer).await?,
            interrupts: region(Window::Interrupts).await?,
            config: region(Window::Config).await?,
        })
    }
}

impl MappedWindows<Window> for Windows {
    fn mapping(&self, window: Window) -> &MmioMapping {
        match window {
            Window::Core => &self.core,
            Window::Channels => &self.channels,
            Window::Observer => &self.observer,
            Window::Interrupts => &self.interrupts,
            Window::Config => &self.config,
        }
    }
}
