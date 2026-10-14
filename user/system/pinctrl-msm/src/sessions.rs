//! Сессии `Pins` и сессии выводов узлов выпускаются на одном порту из общего счётчика значков.
//! Кадр значка сессии узла обслуживает протокол `DevicePins`, остальные кадры - протокол
//! `Pins`.

use component_startup::{SessionEvent, SessionPort};
use ipc::IntoWireValue;
use kernel_objects::{Delivery, Port};
use pinctrl_msm::Line;
use pins_abi::{Claims, Direction, Lines, PinsError, Record, device_pins, pins};
use platform_abi::{Description, DeviceId};
use runtime::Mutex;

use crate::hardware::Hardware;

pub(crate) struct Controller<'d> {
    pub(crate) description: &'d Description<'d>,
    pub(crate) hardware: &'d Mutex<Hardware>,
    pub(crate) claims: Claims,
    pub(crate) lines: Lines<Line>,
}

struct Opener<'a, 'd> {
    sessions: &'a SessionPort,
    controller: &'a mut Controller<'d>,
}

struct Held<'a, 'd> {
    controller: &'a mut Controller<'d>,
    badge: u64,
}

impl Controller<'_> {
    fn release(&mut self, badge: u64) {
        self.hardware.lock().release(badge);
        self.lines.release(badge);
        self.claims.release(badge);
    }
}

impl Opener<'_, '_> {
    fn open(&mut self, device: DeviceId) -> Result<Port, PinsError> {
        let plan = self.controller.hardware.lock().tlmm.plan(
            self.controller.description,
            &device,
            &self.controller.claims,
        )?;
        let minted = self
            .sessions
            .create_session()
            .map_err(|_| PinsError::NoSpace)?;
        let badge = minted.badge.get();

        self.controller.hardware.lock().tlmm.apply(&plan);
        plan.claim(&mut self.controller.claims, badge);
        self.controller.lines.open(badge, device);

        Ok(minted.port)
    }
}

impl pins::Service<Delivery> for Opener<'_, '_> {
    fn open(
        &mut self,
        _frame: &Delivery,
        device: DeviceId,
    ) -> impl IntoWireValue<Delivery, Read = Result<ipc::wire::Session<device_pins::Protocol>, PinsError>>
    {
        // Порт сессии выводов выпускает этот контроллер, и он же обслуживает на нём
        // `DevicePins`.
        Opener::open(self, device).map(kernel_objects::Session::from_handle)
    }
}

impl Held<'_, '_> {
    fn issue(
        &mut self,
        node: DeviceId,
        record: Record<'_>,
        index: u8,
        direction: Direction,
        active: bool,
    ) -> Result<u8, PinsError> {
        let controller = &mut *self.controller;
        let issued = controller
            .lines
            .find(self.badge, &node, record, index, direction)?;
        if let Some(number) = issued {
            if direction == Direction::Output {
                let line = controller.lines.get(self.badge, number)?;
                controller.hardware.lock().tlmm.set(line, active);
            }
            return Ok(number);
        }

        let session = controller
            .lines
            .node(self.badge)
            .ok_or(PinsError::NotIssued)?;
        let hardware = controller.hardware.lock();
        let line = match record {
            Record::Line(name) => {
                hardware
                    .tlmm
                    .line(controller.description, session, &node, name, index)?
            }
            Record::Interrupt => {
                hardware
                    .tlmm
                    .interrupt(controller.description, session, &node, index)?
            }
        };
        if controller
            .claims
            .holder(&line.pin)
            .is_some_and(|holder| holder != self.badge)
        {
            return Err(PinsError::Busy);
        }

        // Выход перенастроил бы вход и его прерывание на том же выводе, даже выданный по другой
        // записи узла.
        let crossed = controller
            .lines
            .issued(self.badge)
            .any(|issued| issued.line.pin == line.pin && issued.direction != direction);
        if crossed {
            return Err(PinsError::Busy);
        }

        match direction {
            Direction::Input => hardware.tlmm.input(&line),
            Direction::Output => hardware.tlmm.output(&line, active),
        }
        drop(hardware);
        controller.claims.claim_pin(self.badge, line.pin.clone());
        controller
            .lines
            .issue(self.badge, node, record, index, direction, line)
    }
}

impl device_pins::Service<Delivery> for Held<'_, '_> {
    fn input(
        &mut self,
        _frame: &Delivery,
        node: DeviceId,
        name: &str,
        index: u8,
    ) -> Result<u8, PinsError> {
        self.issue(node, Record::Line(name), index, Direction::Input, false)
    }

    fn output(
        &mut self,
        _frame: &Delivery,
        node: DeviceId,
        name: &str,
        index: u8,
        active: bool,
    ) -> Result<u8, PinsError> {
        self.issue(node, Record::Line(name), index, Direction::Output, active)
    }

    fn interrupt(&mut self, _frame: &Delivery, node: DeviceId, index: u8) -> Result<u8, PinsError> {
        self.issue(node, Record::Interrupt, index, Direction::Input, false)
    }

    fn active(&mut self, _frame: &Delivery, line: u8) -> Result<bool, PinsError> {
        let line = self.controller.lines.get(self.badge, line)?;
        Ok(self.controller.hardware.lock().tlmm.active(line))
    }

    fn set(&mut self, _frame: &Delivery, line: u8, active: bool) -> Result<(), PinsError> {
        let line = self.controller.lines.output(self.badge, line)?;
        self.controller.hardware.lock().tlmm.set(line, active);
        Ok(())
    }

    fn irq(
        &mut self,
        _frame: &Delivery,
        line: u8,
    ) -> impl IntoWireValue<Delivery, Read = Result<ipc::wire::Cap<syscall::kind::Signal>, PinsError>>
    {
        let input = self.controller.lines.input(self.badge, line)?;
        self.controller.hardware.lock().irq(self.badge, line, input)
    }

    fn ack(&mut self, _frame: &Delivery, line: u8) -> Result<(), PinsError> {
        let input = self.controller.lines.input(self.badge, line)?;
        self.controller.hardware.lock().ack(self.badge, line, input)
    }
}

pub(crate) fn serve(controller: &mut Controller<'_>, sessions: &SessionPort) {
    let mut events = sessions.events();

    while let Some(event) = events.recv() {
        match event {
            SessionEvent::Frame(frame) => {
                let badge = frame.resources.badge;
                if controller.lines.node(badge).is_some() {
                    let mut held = Held { controller, badge };
                    let _ = device_pins::dispatch(&mut held, frame);
                } else {
                    let mut opener = Opener {
                        sessions,
                        controller,
                    };
                    let _ = pins::dispatch(&mut opener, frame);
                }
            }
            SessionEvent::Gone(badge) => controller.release(badge),
        }
    }
}
