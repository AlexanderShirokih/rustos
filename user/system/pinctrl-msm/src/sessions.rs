//! Сессии `Pins` и сессии выводов узлов выпускаются на одном порту из общего счётчика значков.
//! Кадр значка сессии узла обслуживает протокол `DevicePins`, остальные кадры - протокол
//! `Pins`.

use alloc::rc::Rc;
use core::cell::RefCell;

use component_startup::SessionPort;
use executor::{Either, TaskHandle};
use ipc::{Incoming, IntoWireValue, ServiceOwner, wire::IpcError};
use kernel_objects::{Delivery, Port};
use pinctrl_msm::Line;
use pins_abi::{Claims, Direction, Lines, PinsError, Record, device_pins, pins};
use platform_abi::{DeviceId, MappedDescription};

use crate::hardware::Hardware;

pub(crate) struct Controller {
    pub(crate) description: MappedDescription,
    pub(crate) hardware: Rc<RefCell<Hardware>>,
    pub(crate) claims: RefCell<Claims>,
    pub(crate) lines: RefCell<Lines<Line>>,
    pub(crate) sessions: Rc<SessionPort>,
    pub(crate) _interrupts: TaskHandle,
}

#[derive(Clone)]
struct Held {
    controller: Rc<Controller>,
    badge: u64,
}

impl ServiceOwner for Held {
    type Service = Self;

    fn service(&self) -> &Self {
        self
    }
}

impl Controller {
    pub(crate) fn release(&self, badge: u64) {
        self.hardware.borrow_mut().release(badge);
        self.lines.borrow_mut().release(badge);
        self.claims.borrow_mut().release(badge);
    }

    pub(crate) fn serve(
        controller: &Rc<Self>,
        frame: Incoming<'_, Delivery>,
    ) -> Result<Option<impl Future<Output = Result<(), IpcError>> + use<>>, IpcError> {
        let badge = frame.resources.badge;
        let call = if controller.lines.borrow().node(badge).is_some() {
            let held = Held {
                controller: controller.clone(),
                badge,
            };
            device_pins::serve(&held, frame)?.map(Either::Left)
        } else {
            pins::serve(controller, frame)?.map(Either::Right)
        };

        Ok(call.map(Either::run))
    }

    fn open(&self, device: DeviceId) -> Result<Port, PinsError> {
        let description = self
            .description
            .description()
            .map_err(|_| PinsError::NoDevice)?;
        let plan =
            self.hardware
                .borrow()
                .tlmm
                .plan(&description, &device, &self.claims.borrow())?;
        let minted = self
            .sessions
            .create_session()
            .map_err(|_| PinsError::NoSpace)?;
        let badge = minted.badge.get();

        self.hardware.borrow().tlmm.apply(&plan);
        plan.claim(&mut self.claims.borrow_mut(), badge);
        self.lines.borrow_mut().open(badge, device);
        Ok(minted.port)
    }
}

#[allow(clippy::unused_async_trait_impl)]
impl pins::Service<Delivery> for Controller {
    async fn open(
        &self,
        _frame: &Delivery,
        device: DeviceId,
    ) -> impl IntoWireValue<Delivery, Read = Result<ipc::wire::Session<device_pins::Protocol>, PinsError>>
    {
        // Порт сессии выводов выпускает этот контроллер, и он же обслуживает на нём
        // `DevicePins`.
        Controller::open(self, device).map(kernel_objects::Session::from_handle)
    }
}

impl Held {
    fn issue(
        &self,
        node: DeviceId,
        record: Record<'_>,
        index: u8,
        direction: Direction,
        active: bool,
    ) -> Result<u8, PinsError> {
        let controller = &self.controller;
        let description = controller
            .description
            .description()
            .map_err(|_| PinsError::NoDevice)?;
        let mut lines = controller.lines.borrow_mut();
        let mut claims = controller.claims.borrow_mut();
        let hardware = controller.hardware.borrow();

        let issued = lines.find(self.badge, &node, record, index, direction)?;
        if let Some(number) = issued {
            if direction == Direction::Output {
                let line = lines.get(self.badge, number)?;
                hardware.tlmm.set(line, active);
            }

            return Ok(number);
        }

        let session = lines.node(self.badge).ok_or(PinsError::NotIssued)?;

        let line = match record {
            Record::Line(name) => hardware
                .tlmm
                .line(&description, session, &node, name, index)?,
            Record::Interrupt => hardware
                .tlmm
                .interrupt(&description, session, &node, index)?,
        };

        if claims
            .holder(&line.pin)
            .is_some_and(|holder| holder != self.badge)
        {
            return Err(PinsError::Busy);
        }

        // Выход перенастроил бы вход и его прерывание на том же выводе, даже выданный по другой
        // записи узла.
        let crossed = lines
            .issued(self.badge)
            .any(|issued| issued.line.pin == line.pin && issued.direction != direction);
        if crossed {
            return Err(PinsError::Busy);
        }

        match direction {
            Direction::Input => hardware.tlmm.input(&line),
            Direction::Output => hardware.tlmm.output(&line, active),
        }

        claims.claim_pin(self.badge, line.pin.clone());
        lines.issue(self.badge, node, record, index, direction, line)
    }
}

#[allow(clippy::unused_async_trait_impl)]
impl device_pins::Service<Delivery> for Held {
    async fn input(
        &self,
        _frame: &Delivery,
        node: DeviceId,
        name: &str,
        index: u8,
    ) -> Result<u8, PinsError> {
        self.issue(node, Record::Line(name), index, Direction::Input, false)
    }

    fn output(
        &self,
        _frame: &Delivery,
        node: DeviceId,
        name: &str,
        index: u8,
        active: bool,
    ) -> Result<u8, PinsError> {
        self.issue(node, Record::Line(name), index, Direction::Output, active)
    }

    fn interrupt(&self, _frame: &Delivery, node: DeviceId, index: u8) -> Result<u8, PinsError> {
        self.issue(node, Record::Interrupt, index, Direction::Input, false)
    }

    async fn active(&self, _frame: &Delivery, line: u8) -> Result<bool, PinsError> {
        let lines = self.controller.lines.borrow();
        let line = lines.get(self.badge, line)?;
        Ok(self.controller.hardware.borrow().tlmm.active(line))
    }

    fn set(&self, _frame: &Delivery, line: u8, active: bool) -> Result<(), PinsError> {
        let lines = self.controller.lines.borrow();
        let line = lines.output(self.badge, line)?;
        self.controller.hardware.borrow().tlmm.set(line, active);
        Ok(())
    }

    async fn irq(
        &self,
        _frame: &Delivery,
        line: u8,
    ) -> impl IntoWireValue<Delivery, Read = Result<ipc::wire::Cap<syscall::kind::Signal>, PinsError>>
    {
        let lines = self.controller.lines.borrow();
        let input = lines.input(self.badge, line)?;
        self.controller
            .hardware
            .borrow_mut()
            .irq(self.badge, line, input)
    }

    async fn ack(&self, _frame: &Delivery, line: u8) -> Result<(), PinsError> {
        let lines = self.controller.lines.borrow();
        let input = lines.input(self.badge, line)?;
        self.controller
            .hardware
            .borrow()
            .ack(self.badge, line, input)
    }
}
