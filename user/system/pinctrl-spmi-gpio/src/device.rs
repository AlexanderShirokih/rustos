//! Устройство контроллера, выданное шиной SPMI.

use component_startup::{AnyOf, Providers};
use ipc::CallError;
use kernel_objects::{Session, SessionTransport, Signal};
use pinctrl_spmi_gpio::{ControllerNode, Registers};
use pins_abi::PinsError;
use platform_abi::{CONTROLLER_KEY, Description};
use runtime::Timeout;
use spmi_abi::{DeviceError, Trigger, spmi, spmi_device};

const BUS_CALL_TIMEOUT: Timeout = Timeout::from_secs(2);

const PIN_INTERRUPT_BIT: u8 = 0;

pub(crate) struct Device {
    _bus: spmi::Client<SessionTransport>,
    device: spmi_device::Client<SessionTransport>,
}

impl Device {
    pub(crate) fn bind(
        description: &Description<'_>,
        spmi: &Providers<spmi::Protocol>,
        controller: &ControllerNode,
    ) -> Result<Self, PinsError> {
        let bus = spmi_abi::controller(description, &controller.id).ok_or(PinsError::Failed)?;
        let compatible = bus
            .strings("compatible")
            .ok()
            .flatten()
            .ok_or(PinsError::Failed)?;

        let found = spmi
            .all(AnyOf::new(CONTROLLER_KEY, compatible))
            .ok()
            .and_then(|mut found| found.next())
            .and_then(Result::ok)
            .ok_or(PinsError::Failed)?;
        let bus = spmi
            .open(&found)
            .map_err(|_| PinsError::Failed)?
            .client()
            .with_wait(BUS_CALL_TIMEOUT);
        let device = bus
            .open_blocks(controller.id.as_str(), controller.pins())
            .map_err(|_| PinsError::Failed)?;

        Ok(Self {
            _bus: bus,
            device: Session::<spmi_device::Protocol>::from_session(device)
                .client()
                .with_wait(BUS_CALL_TIMEOUT),
        })
    }

    pub(crate) fn irq(&self, block: u8) -> Result<Signal, PinsError> {
        self.device
            .block_irq(block, PIN_INTERRUPT_BIT, Trigger::Both)
            .map(Signal::from)
            .map_err(irq_refused)
    }

    pub(crate) fn ack(&self, block: u8) -> Result<(), PinsError> {
        self.device
            .block_ack(block, PIN_INTERRUPT_BIT)
            .map_err(irq_refused)
    }

    pub(crate) fn release(&self, block: u8) {
        let _ = self.device.block_release(block, PIN_INTERRUPT_BIT);
    }
}

impl Registers for Device {
    fn read(&self, address: u16) -> Result<u8, DeviceError> {
        let value = self.device.read(address, 1).map_err(remote)?;
        u8::try_from(value).map_err(|_| DeviceError::Failed)
    }

    fn update(&self, address: u16, mask: u8, value: u8) -> Result<(), DeviceError> {
        self.device.update(address, mask, value).map_err(remote)
    }
}

fn remote(error: CallError<DeviceError>) -> DeviceError {
    match error {
        CallError::Remote(error) => error,
        CallError::Ipc(_) => DeviceError::Failed,
    }
}

fn irq_refused(error: CallError<DeviceError>) -> PinsError {
    match remote(error) {
        DeviceError::Denied => PinsError::Denied,
        DeviceError::Unavailable => PinsError::NoSpace,
        _ => PinsError::Failed,
    }
}
