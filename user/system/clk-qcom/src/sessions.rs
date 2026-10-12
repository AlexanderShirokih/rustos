//! Сессии `Clocks`: значок сессии занимает включённые ею узлы.

use clk_qcom::Gcc;
use clocks_abi::{ClocksError, clocks};
use component_startup::{SessionEvent, SessionPort};
use kernel_objects::Delivery;
use platform_abi::{Description, DeviceClaims, DeviceId};

use crate::window::Window;

pub(crate) struct Controller<'d> {
    pub(crate) description: &'d Description<'d>,
    pub(crate) gcc: Gcc<Window>,
    pub(crate) claims: DeviceClaims,
}

impl clocks::Service<Delivery> for Controller<'_> {
    fn enable(&mut self, frame: &Delivery, device: DeviceId) -> Result<(), ClocksError> {
        self.gcc
            .enable(self.description, &mut self.claims, frame.badge, &device)
    }

    fn set_rate(
        &mut self,
        frame: &Delivery,
        device: DeviceId,
        name: &str,
        hz: u64,
    ) -> Result<(), ClocksError> {
        self.gcc.set_rate(
            self.description,
            &self.claims,
            frame.badge,
            &device,
            name,
            hz,
        )
    }
}

pub(crate) fn serve(controller: &mut Controller<'_>, sessions: &SessionPort) {
    let mut events = sessions.events();

    while let Some(event) = events.recv() {
        match event {
            SessionEvent::Frame(frame) => {
                let _ = clocks::dispatch(controller, frame);
            }
            SessionEvent::Gone(badge) => controller.claims.release(badge),
        }
    }
}
