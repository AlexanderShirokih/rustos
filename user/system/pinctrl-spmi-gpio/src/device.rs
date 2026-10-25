//! Устройство контроллера, выданное шиной SPMI.

use component_startup::{AnyOf, Providers};
use executor::{PortChannel, Reactor};
use ipc::CallError;
use kernel_objects::{Session, SessionTransport};
use pinctrl_spmi_gpio::{ControllerNode, Registers};
use pins_abi::PinsError;
use platform_abi::{CONTROLLER_KEY, Description};
use runtime::Timeout;
use spmi_abi::{DeviceError, Trigger, spmi, spmi_device, spmi_interrupt};

const BUS_CALL_TIMEOUT: Timeout = Timeout::from_secs(2);

const PIN_INTERRUPT_BIT: u8 = 0;

pub(crate) struct Device {
    _bus: spmi::Client<PortChannel<SessionTransport>>,
    device: spmi_device::Client<PortChannel<SessionTransport>>,
}

impl Device {
    pub(crate) async fn bind(
        description: &Description<'_>,
        spmi: &Providers<spmi::Protocol>,
        controller: &ControllerNode,
        reactor: &Reactor,
    ) -> Result<Self, PinsError> {
        let bus = spmi_abi::controller(description, &controller.id).ok_or(PinsError::Failed)?;
        let compatible = bus
            .strings("compatible")
            .ok()
            .flatten()
            .ok_or(PinsError::Failed)?;

        let session = spmi
            .open_first(reactor, AnyOf::new(CONTROLLER_KEY, compatible))
            .await
            .map_err(|_| PinsError::Failed)?
            .ok_or(PinsError::Failed)?;

        let bus = PortChannel::client(session, reactor.clone()).with_wait(BUS_CALL_TIMEOUT);
        let device = bus
            .open_blocks(controller.id.clone(), controller.pins())
            .await
            .map_err(|_| PinsError::Failed)?;

        Ok(Self {
            _bus: bus,
            device: PortChannel::client(
                Session::<spmi_device::Protocol>::from_session(device),
                reactor.clone(),
            )
            .with_wait(BUS_CALL_TIMEOUT),
        })
    }

    pub(crate) async fn irq(
        &self,
        block: u8,
        reactor: &Reactor,
    ) -> Result<spmi_interrupt::Client<PortChannel<SessionTransport>>, PinsError> {
        let interrupt = self
            .device
            .block_irq(block, PIN_INTERRUPT_BIT, Trigger::Both)
            .await
            .map_err(irq_refused)?;

        Ok(PortChannel::client(
            Session::<spmi_interrupt::Protocol>::from_session(interrupt),
            reactor.clone(),
        )
        .with_wait(BUS_CALL_TIMEOUT))
    }
}

impl Registers for Device {
    async fn read(&self, address: u16) -> Result<u8, DeviceError> {
        let value = self.device.read(address, 1).await.map_err(remote)?;
        u8::try_from(value).map_err(|_| DeviceError::Failed)
    }

    async fn update(&self, address: u16, mask: u8, value: u8) -> Result<(), DeviceError> {
        self.device
            .update(address, mask, value)
            .await
            .map_err(remote)
    }
}

fn remote(error: CallError<DeviceError>) -> DeviceError {
    match error {
        CallError::Remote(error) => error,
        CallError::Ipc(_) => DeviceError::Failed,
    }
}

pub(crate) fn irq_refused(error: CallError<DeviceError>) -> PinsError {
    match remote(error) {
        DeviceError::Denied => PinsError::Denied,
        DeviceError::Unavailable => PinsError::NoSpace,
        DeviceError::Busy => PinsError::Busy,
        _ => PinsError::Failed,
    }
}
