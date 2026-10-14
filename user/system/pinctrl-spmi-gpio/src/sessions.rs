//! Сессии `Pins` и сессии выводов узлов выпускаются на одном порту из общего счётчика значков.
//! Кадр значка сессии узла обслуживает протокол `DevicePins`, остальные кадры - протокол `Pins`.

use alloc::vec::Vec;

use component_startup::{Providers, SessionEvent, SessionPort};
use ipc::IntoWireValue;
use kernel_objects::{Delivery, Port, SessionTransport, Signal};
use pinctrl_spmi_gpio::{ControllerNode, Gpio, Line, line, plan};
use pins_abi::{Claims, Direction, Lines, PinsError, Record, device_pins, pins};
use platform_abi::{Description, DeviceId};
use runtime::Mutex;
use spmi_abi::{spmi, spmi_interrupt};

use crate::device::{Device, irq_refused};

pub(crate) struct Driver<'d> {
    pub(crate) description: &'d Description<'d>,
    pub(crate) spmi: &'d Providers<spmi::Protocol>,
    pub(crate) gpios: Vec<(DeviceId, Gpio<Device>)>,
    pub(crate) claims: Claims,
    pub(crate) lines: Lines<Input>,
}

pub(crate) struct Input {
    line: Line,
    irq: Option<spmi_interrupt::Client<SessionTransport>>,
}

struct Opener<'a, 'd> {
    sessions: &'a SessionPort,
    driver: &'a mut Driver<'d>,
}

struct Held<'a, 'd> {
    driver: &'a mut Driver<'d>,
    badge: u64,
}

impl Driver<'_> {
    fn gpio(&mut self, controller: &ControllerNode) -> Result<&mut Gpio<Device>, PinsError> {
        let index = if let Some(index) = self.gpios.iter().position(|(id, _)| *id == controller.id)
        {
            index
        } else {
            let device = Device::bind(self.description, self.spmi, controller)?;
            self.gpios.push((controller.id.clone(), Gpio::new(device)));
            self.gpios.len().saturating_sub(1)
        };

        self.gpios
            .get_mut(index)
            .map(|(_, gpio)| gpio)
            .ok_or(PinsError::Failed)
    }

    fn release(&mut self, badge: u64) {
        // Сброс линий закрывает сессии их прерываний, и шина отзывает их `Signal`.
        self.lines.release(badge);
        self.claims.release(badge);
    }
}

impl Opener<'_, '_> {
    fn open(&mut self, device: DeviceId) -> Result<Port, PinsError> {
        let driver = &mut *self.driver;
        let plan = plan(driver.description, &device, &driver.claims)?;
        for controller in plan.controllers() {
            let settings = plan.settings(&controller.id);
            driver.gpio(controller)?.apply(&settings)?;
        }

        let minted = self
            .sessions
            .create_session()
            .map_err(|_| PinsError::NoSpace)?;
        let badge = minted.badge.get();

        plan.claim(&mut driver.claims, badge);
        driver.lines.open(badge, device);
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
    fn line(&self, node: &DeviceId, name: &str, index: u8) -> Result<Line, PinsError> {
        let session = self
            .driver
            .lines
            .node(self.badge)
            .ok_or(PinsError::NotIssued)?;
        let line = line(self.driver.description, session, node, name, index)?;
        if self
            .driver
            .claims
            .holder(&line.pin())
            .is_some_and(|holder| holder != self.badge)
        {
            return Err(PinsError::Busy);
        }
        Ok(line)
    }

    fn issue(&mut self, node: DeviceId, name: &str, index: u8) -> Result<u8, PinsError> {
        let lines = &self.driver.lines;
        let record = Record::Line(name);
        if let Some(number) = lines.find(self.badge, &node, record, index, Direction::Input)? {
            return Ok(number);
        }

        let line = self.line(&node, name, index)?;
        self.driver.gpio(&line.controller)?.input(line.block)?;
        self.driver.claims.claim_pin(self.badge, line.pin());
        self.driver.lines.issue(
            self.badge,
            node,
            record,
            index,
            Direction::Input,
            Input { line, irq: None },
        )
    }

    fn issued(&self, line: u8) -> Result<&Input, PinsError> {
        self.driver.lines.get(self.badge, line)
    }

    fn signal(&mut self, line: u8) -> Result<Signal, PinsError> {
        let input = self.issued(line)?;
        if input.irq.is_none() {
            let (controller, block) = (input.line.controller.clone(), input.line.block);
            let interrupt = self.driver.gpio(&controller)?.registers().irq(block)?;
            self.driver.lines.get_mut(self.badge, line)?.irq = Some(interrupt);
        }

        self.irq_session(line)?
            .signal()
            .map(Signal::from)
            .map_err(irq_refused)
    }

    fn irq_session(
        &self,
        line: u8,
    ) -> Result<&spmi_interrupt::Client<SessionTransport>, PinsError> {
        self.issued(line)?.irq.as_ref().ok_or(PinsError::NotIssued)
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
        self.issue(node, name, index)
    }

    fn output(
        &mut self,
        _frame: &Delivery,
        node: DeviceId,
        name: &str,
        index: u8,
        _active: bool,
    ) -> Result<u8, PinsError> {
        // Выход линии PMIC не выдаётся, это текущее ограничение.
        self.line(&node, name, index)?;
        Err(PinsError::Unsupported)
    }

    fn interrupt(&mut self, _frame: &Delivery, node: DeviceId, index: u8) -> Result<u8, PinsError> {
        // Линия по записи прерывания PMIC не выдаётся, это текущее ограничение.
        let session = self
            .driver
            .lines
            .node(self.badge)
            .ok_or(PinsError::NotIssued)?;
        pins_abi::interrupt(self.driver.description, session, &node, index)
            .ok_or(PinsError::NoDevice)?;
        Err(PinsError::Unsupported)
    }

    fn active(&mut self, _frame: &Delivery, line: u8) -> Result<bool, PinsError> {
        let line = self.issued(line)?.line.clone();
        let high = self.driver.gpio(&line.controller)?.level(line.block)?;
        Ok(line.active(high))
    }

    fn set(&mut self, _frame: &Delivery, _line: u8, _active: bool) -> Result<(), PinsError> {
        Err(PinsError::NotIssued)
    }

    fn irq(
        &mut self,
        _frame: &Delivery,
        line: u8,
    ) -> impl IntoWireValue<Delivery, Read = Result<ipc::wire::Cap<syscall::kind::Signal>, PinsError>>
    {
        self.signal(line)
    }

    fn ack(&mut self, _frame: &Delivery, line: u8) -> Result<(), PinsError> {
        self.irq_session(line)?.ack().map_err(irq_refused)
    }
}

pub(crate) fn serve(driver: &Mutex<Driver<'_>>, sessions: &SessionPort) {
    let mut events = sessions.events();

    while let Some(event) = events.recv() {
        let mut driver = driver.lock();
        match event {
            SessionEvent::Frame(frame) => {
                let badge = frame.resources.badge;
                if driver.lines.node(badge).is_some() {
                    let mut held = Held {
                        driver: &mut driver,
                        badge,
                    };
                    let _ = device_pins::dispatch(&mut held, frame);
                } else {
                    let mut opener = Opener {
                        sessions,
                        driver: &mut driver,
                    };
                    let _ = pins::dispatch(&mut opener, frame);
                }
            }
            SessionEvent::Gone(badge) => driver.release(badge),
        }
    }
}
