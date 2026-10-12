//! Сессии `Pins` и сессии выводов узлов выпускаются на одном порту из общего счётчика значков.
//! Кадр значка сессии узла обслуживает протокол `DevicePins`, остальные кадры - протокол `Pins`.

use alloc::{collections::BTreeMap, string::String, vec::Vec};

use component_startup::{Providers, SessionEvent, SessionPort};
use ipc::IntoWireValue;
use kernel_objects::{Delivery, Port, Signal};
use pinctrl_spmi_gpio::{ControllerNode, Gpio, Line, line, plan};
use pins_abi::{Claims, PinsError, device_pins, pins};
use platform_abi::{Description, DeviceId};
use runtime::Mutex;
use spmi_abi::spmi;

use crate::device::Device;

pub(crate) struct Driver<'d> {
    pub(crate) description: &'d Description<'d>,
    pub(crate) spmi: &'d Providers<spmi::Protocol>,
    pub(crate) gpios: Vec<(DeviceId, Gpio<Device>)>,
    pub(crate) claims: Claims,
    pub(crate) sessions: BTreeMap<u64, NodeSession>,
}

pub(crate) struct NodeSession {
    node: DeviceId,
    lines: Vec<Issued>,
}

struct Issued {
    node: DeviceId,
    property: String,
    index: u8,
    line: Line,
    irq: bool,
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
        if let Some(session) = self.sessions.remove(&badge) {
            for issued in session.lines.iter().filter(|issued| issued.irq) {
                let gpio = self
                    .gpios
                    .iter()
                    .find(|(id, _)| *id == issued.line.controller.id);
                if let Some((_, gpio)) = gpio {
                    gpio.registers().release(issued.line.block);
                }
            }
        }
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
        driver.sessions.insert(
            badge,
            NodeSession {
                node: device,
                lines: Vec::new(),
            },
        );
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
    fn issued(&self, line: u8) -> Result<&Issued, PinsError> {
        self.driver
            .sessions
            .get(&self.badge)
            .and_then(|session| session.lines.get(usize::from(line)))
            .ok_or(PinsError::NotIssued)
    }

    fn issue(&mut self, node: DeviceId, property: &str, index: u8) -> Result<u8, PinsError> {
        let session = self
            .driver
            .sessions
            .get(&self.badge)
            .ok_or(PinsError::NotIssued)?;

        let issued = session.lines.iter().position(|issued| {
            issued.node == node && issued.property == property && issued.index == index
        });
        if let Some(issued) = issued {
            return u8::try_from(issued).map_err(|_| PinsError::NoSpace);
        }

        let number = u8::try_from(session.lines.len()).map_err(|_| PinsError::NoSpace)?;
        let line = line(
            self.driver.description,
            &session.node,
            &node,
            property,
            index,
        )?;
        let pin = line.pin();
        if self
            .driver
            .claims
            .holder(&pin)
            .is_some_and(|holder| holder != self.badge)
        {
            return Err(PinsError::Busy);
        }
        self.driver.gpio(&line.controller)?.input(line.block)?;
        self.driver.claims.claim_pin(self.badge, pin);

        if let Some(session) = self.driver.sessions.get_mut(&self.badge) {
            session.lines.push(Issued {
                node,
                property: property.into(),
                index,
                line,
                irq: false,
            });
        }
        Ok(number)
    }

    fn signal(&mut self, line: u8) -> Result<Signal, PinsError> {
        let issued = self.issued(line)?;
        let (controller, block) = (issued.line.controller.clone(), issued.line.block);
        let signal = self.driver.gpio(&controller)?.registers().irq(block)?;

        let issued = self
            .driver
            .sessions
            .get_mut(&self.badge)
            .and_then(|session| session.lines.get_mut(usize::from(line)));
        if let Some(issued) = issued {
            issued.irq = true;
        }
        Ok(signal)
    }
}

impl device_pins::Service<Delivery> for Held<'_, '_> {
    fn input(
        &mut self,
        _frame: &Delivery,
        node: DeviceId,
        property: &str,
        index: u8,
    ) -> Result<u8, PinsError> {
        self.issue(node, property, index)
    }

    fn active(&mut self, _frame: &Delivery, line: u8) -> Result<bool, PinsError> {
        let line = self.issued(line)?.line.clone();
        let high = self.driver.gpio(&line.controller)?.level(line.block)?;
        Ok(line.active(high))
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
        let issued = self.issued(line)?;
        if !issued.irq {
            return Err(PinsError::NotIssued);
        }
        let (controller, block) = (issued.line.controller.clone(), issued.line.block);
        self.driver.gpio(&controller)?.registers().ack(block)
    }
}

pub(crate) fn serve(driver: &Mutex<Driver<'_>>, sessions: &SessionPort) {
    let mut events = sessions.events();

    while let Some(event) = events.recv() {
        let mut driver = driver.lock();
        match event {
            SessionEvent::Frame(frame) => {
                let badge = frame.resources.badge;
                if driver.sessions.contains_key(&badge) {
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
