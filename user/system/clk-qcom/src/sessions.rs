//! Сессии `Clocks`: значок сессии занимает включённые ею узлы.

use core::cell::RefCell;

use clk_qcom::Gcc;
use clocks_abi::{ClocksError, clocks};
use kernel_objects::{Delivery, MmioMapping};
use platform_abi::{DeviceClaims, DeviceId, MappedDescription};

pub(crate) struct Controller {
    pub(crate) description: MappedDescription,
    pub(crate) gcc: Gcc<MmioMapping>,
    pub(crate) claims: RefCell<DeviceClaims>,
}

#[allow(clippy::unused_async_trait_impl)]
impl clocks::Service<Delivery> for Controller {
    fn enable(&self, frame: &Delivery, device: DeviceId) -> Result<(), ClocksError> {
        let description = self
            .description
            .description()
            .map_err(|_| ClocksError::NoDevice)?;
        self.gcc.enable(
            &description,
            &mut self.claims.borrow_mut(),
            frame.badge,
            &device,
        )
    }

    fn set_rate(
        &self,
        frame: &Delivery,
        device: DeviceId,
        name: &str,
        hz: u64,
    ) -> Result<(), ClocksError> {
        let description = self
            .description
            .description()
            .map_err(|_| ClocksError::NoDevice)?;
        self.gcc.set_rate(
            &description,
            &self.claims.borrow(),
            frame.badge,
            &device,
            name,
            hz,
        )
    }
}
