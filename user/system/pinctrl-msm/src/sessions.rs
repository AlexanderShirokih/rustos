//! Сессии `Pins` и сессии выводов узлов выпускаются на одном порту из общего счётчика значков.
//! Кадр значка, которым заняты выводы узла, обслуживает протокол `DevicePins`, остальные
//! кадры - протокол `Pins`.

use component_startup::{SessionEvent, SessionPort};
use ipc::IntoWireValue;
use kernel_objects::{Delivery, Port, Signal};
use pinctrl_msm::Tlmm;
use pins_abi::{Claims, PinsError, device_pins, pins};
use platform_abi::Description;

use crate::windows::Windows;

pub(crate) struct Controller<'d> {
    pub(crate) description: &'d Description<'d>,
    pub(crate) tlmm: Tlmm<Windows>,
    pub(crate) claims: Claims,
}

struct Opener<'a, 'd> {
    sessions: &'a SessionPort,
    controller: &'a mut Controller<'d>,
}

struct Held;

impl Opener<'_, '_> {
    fn open(&mut self, device: &str) -> Result<Port, PinsError> {
        let plan = self.controller.tlmm.plan(
            self.controller.description,
            device,
            &self.controller.claims,
        )?;
        let minted = self
            .sessions
            .create_session()
            .map_err(|_| PinsError::NoSpace)?;

        self.controller.tlmm.apply(&plan);
        plan.claim(&mut self.controller.claims, minted.badge.get());

        Ok(minted.port)
    }
}

impl pins::Service<Delivery> for Opener<'_, '_> {
    fn open(
        &mut self,
        _frame: &Delivery,
        device: &str,
    ) -> impl IntoWireValue<Delivery, Read = Result<ipc::wire::Session<device_pins::Protocol>, PinsError>>
    {
        // Порт сессии выводов выпускает этот контроллер, и он же обслуживает на нём
        // `DevicePins`.
        Opener::open(self, device).map(kernel_objects::Session::from_handle)
    }
}

impl device_pins::Service<Delivery> for Held {
    fn input(
        &mut self,
        _frame: &Delivery,
        _node: &str,
        _property: &str,
        _index: u8,
    ) -> Result<u8, PinsError> {
        // Линии TLMM контроллер не выдаёт, это текущее ограничение.
        Err(PinsError::Unsupported)
    }

    fn active(&mut self, _frame: &Delivery, _line: u8) -> Result<bool, PinsError> {
        Err(PinsError::NotIssued)
    }

    fn irq(
        &mut self,
        _frame: &Delivery,
        _line: u8,
    ) -> impl IntoWireValue<Delivery, Read = Result<ipc::wire::Cap<syscall::kind::Signal>, PinsError>>
    {
        Err::<Signal, _>(PinsError::NotIssued)
    }

    fn ack(&mut self, _frame: &Delivery, _line: u8) -> Result<(), PinsError> {
        Err(PinsError::NotIssued)
    }
}

pub(crate) fn serve(controller: &mut Controller<'_>, sessions: &SessionPort) {
    let mut events = sessions.events();

    while let Some(event) = events.recv() {
        match event {
            SessionEvent::Frame(frame) => {
                if controller.claims.holds_session(frame.resources.badge) {
                    let _ = device_pins::dispatch(&mut Held, frame);
                } else {
                    let mut opener = Opener {
                        sessions,
                        controller,
                    };
                    let _ = pins::dispatch(&mut opener, frame);
                }
            }
            SessionEvent::Gone(badge) => controller.claims.release(badge),
        }
    }
}
