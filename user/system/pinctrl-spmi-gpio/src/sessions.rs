//! Сессии Pins и DevicePins на общем порту контроллера.

use alloc::{rc::Rc, vec::Vec};
use core::cell::RefCell;

use component_startup::{Providers, SessionPort};
use executor::{Either, Mutex, MutexGuard, PortChannel, Reactor};
use ipc::{Incoming, IntoWireValue, ServiceOwner, wire::IpcError};
use kernel_objects::{Delivery, Port, SessionTransport, Signal};
use pinctrl_spmi_gpio::{ControllerNode, Gpio, Line, line, plan};
use pins_abi::{Claims, Direction, Lines, PinsError, Record, device_pins, pins};
use platform_abi::{DeviceId, MappedDescription};
use spmi_abi::{spmi, spmi_interrupt};

use crate::device::{Device, irq_refused};

type Interrupt = spmi_interrupt::Client<PortChannel<SessionTransport>>;

pub(crate) struct Driver {
    description: MappedDescription,
    spmi: Providers<spmi::Protocol>,
    sessions: Rc<SessionPort>,
    reactor: Reactor,
    gpios: RefCell<Vec<Rc<Slot>>>,
    claims: RefCell<Claims>,
    lines: RefCell<Lines<Rc<Input>>>,
}

struct Slot {
    controller: ControllerNode,
    gpio: Mutex<Option<Gpio<Device>>>,
}

struct Input {
    line: Line,
    irq: Mutex<Option<Rc<Interrupt>>>,
}

#[derive(Clone)]
struct Held {
    driver: Rc<Driver>,
    badge: u64,
}

impl ServiceOwner for Held {
    type Service = Self;

    fn service(&self) -> &Self {
        self
    }
}

impl Driver {
    pub(crate) fn new(
        description: MappedDescription,
        spmi: Providers<spmi::Protocol>,
        sessions: Rc<SessionPort>,
        reactor: Reactor,
    ) -> Self {
        Self {
            description,
            spmi,
            sessions,
            reactor,
            gpios: RefCell::new(Vec::new()),
            claims: RefCell::new(Claims::new()),
            lines: RefCell::new(Lines::new()),
        }
    }

    fn slot(&self, controller: &ControllerNode) -> Rc<Slot> {
        let mut gpios = self.gpios.borrow_mut();
        if let Some(slot) = gpios
            .iter()
            .find(|slot| slot.controller.id == controller.id)
        {
            return slot.clone();
        }

        let slot = Rc::new(Slot {
            controller: controller.clone(),
            gpio: Mutex::new(None),
        });
        gpios.push(slot.clone());
        slot
    }

    pub(crate) fn release(&self, badge: u64) {
        self.lines.borrow_mut().release(badge);
        self.claims.borrow_mut().release(badge);
    }

    pub(crate) fn serve(
        driver: &Rc<Self>,
        frame: Incoming<'_, Delivery>,
    ) -> Result<Option<impl Future<Output = Result<(), IpcError>> + use<>>, IpcError> {
        let badge = frame.resources.badge;
        let held = driver.lines.borrow().node(badge).is_some();
        let call = if held {
            let held = Held {
                driver: driver.clone(),
                badge,
            };
            device_pins::serve(&held, frame)?.map(Either::Left)
        } else {
            pins::serve(driver, frame)?.map(Either::Right)
        };

        Ok(call.map(Either::run))
    }

    async fn open(&self, device: DeviceId) -> Result<Port, PinsError> {
        let description = self
            .description
            .description()
            .map_err(|_| PinsError::NoDevice)?;
        let checked = plan(&description, &device, &self.claims.borrow())?;
        let mut slots: Vec<_> = checked
            .controllers()
            .iter()
            .map(|controller| self.slot(controller))
            .collect();
        slots.sort_by(|left, right| {
            left.controller
                .id
                .as_str()
                .cmp(right.controller.id.as_str())
        });

        let mut locked = Vec::with_capacity(slots.len());
        for slot in &slots {
            locked.push((&slot.controller.id, slot.lock(self).await?));
        }

        let checked = plan(&description, &device, &self.claims.borrow())?;
        for controller in checked.controllers() {
            let settings = checked.settings(&controller.id);
            let (_, gpio) = locked
                .iter_mut()
                .find(|(id, _)| **id == controller.id)
                .ok_or(PinsError::Failed)?;
            gpio.as_mut()
                .ok_or(PinsError::Failed)?
                .apply(&settings)
                .await?;
        }

        let minted = self
            .sessions
            .create_session()
            .map_err(|_| PinsError::NoSpace)?;
        let badge = minted.badge.get();
        checked.claim(&mut self.claims.borrow_mut(), badge);
        self.lines.borrow_mut().open(badge, device);
        Ok(minted.port)
    }
}

impl Slot {
    async fn lock<'a>(
        &'a self,
        driver: &Driver,
    ) -> Result<MutexGuard<'a, Option<Gpio<Device>>>, PinsError> {
        let mut gpio = self.gpio.lock().await;
        if gpio.is_none() {
            let description = driver
                .description
                .description()
                .map_err(|_| PinsError::NoDevice)?;
            let device = Device::bind(
                &description,
                &driver.spmi,
                &self.controller,
                &driver.reactor,
            )
            .await?;
            *gpio = Some(Gpio::new(device));
        }

        Ok(gpio)
    }
}

impl Held {
    fn line(&self, node: &DeviceId, name: &str, index: u8) -> Result<Line, PinsError> {
        let lines = self.driver.lines.borrow();
        let session = lines.node(self.badge).ok_or(PinsError::NotIssued)?;
        let description = self
            .driver
            .description
            .description()
            .map_err(|_| PinsError::NoDevice)?;
        let line = line(&description, session, node, name, index)?;
        if self
            .driver
            .claims
            .borrow()
            .holder(&line.pin())
            .is_some_and(|holder| holder != self.badge)
        {
            return Err(PinsError::Busy);
        }

        Ok(line)
    }

    async fn issue(&self, node: DeviceId, name: &str, index: u8) -> Result<u8, PinsError> {
        let record = Record::Line(name);
        if let Some(number) =
            self.driver
                .lines
                .borrow()
                .find(self.badge, &node, record, index, Direction::Input)?
        {
            return Ok(number);
        }

        let line = self.line(&node, name, index)?;
        let slot = self.driver.slot(&line.controller);
        let mut gpio = slot.lock(&self.driver).await?;
        if let Some(number) =
            self.driver
                .lines
                .borrow()
                .find(self.badge, &node, record, index, Direction::Input)?
        {
            return Ok(number);
        }

        let line = self.line(&node, name, index)?;
        gpio.as_mut()
            .ok_or(PinsError::Failed)?
            .input(line.block)
            .await?;
        self.driver
            .claims
            .borrow_mut()
            .claim_pin(self.badge, line.pin());
        self.driver.lines.borrow_mut().issue(
            self.badge,
            node,
            record,
            index,
            Direction::Input,
            Rc::new(Input {
                line,
                irq: Mutex::new(None),
            }),
        )
    }

    fn issued(&self, number: u8) -> Result<Rc<Input>, PinsError> {
        self.driver.lines.borrow().get(self.badge, number).cloned()
    }

    async fn signal(&self, number: u8) -> Result<Signal, PinsError> {
        let input = self.issued(number)?;
        let irq = {
            let mut irq = input.irq.lock().await;
            if irq.is_none() {
                let slot = self.driver.slot(&input.line.controller);
                let gpio = slot.lock(&self.driver).await?;
                let gpio = gpio.as_ref().ok_or(PinsError::Failed)?;
                let interrupt = gpio
                    .registers()
                    .irq(input.line.block, &self.driver.reactor)
                    .await?;

                *irq = Some(Rc::new(interrupt));
            }

            irq.as_ref().cloned().ok_or(PinsError::NotIssued)?
        };

        irq.signal().await.map(Signal::from).map_err(irq_refused)
    }
}

impl pins::Service<Delivery> for Driver {
    async fn open(
        &self,
        _frame: &Delivery,
        device: DeviceId,
    ) -> impl IntoWireValue<Delivery, Read = Result<ipc::wire::Session<device_pins::Protocol>, PinsError>>
    {
        Driver::open(self, device)
            .await
            .map(kernel_objects::Session::from_handle)
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
        self.issue(node, name, index).await
    }

    fn output(
        &self,
        _frame: &Delivery,
        node: DeviceId,
        name: &str,
        index: u8,
        _active: bool,
    ) -> Result<u8, PinsError> {
        self.line(&node, name, index)?;
        Err(PinsError::Unsupported)
    }

    fn interrupt(&self, _frame: &Delivery, node: DeviceId, index: u8) -> Result<u8, PinsError> {
        let lines = self.driver.lines.borrow();
        let session = lines.node(self.badge).ok_or(PinsError::NotIssued)?;
        let description = self
            .driver
            .description
            .description()
            .map_err(|_| PinsError::NoDevice)?;
        pins_abi::interrupt(&description, session, &node, index).ok_or(PinsError::NoDevice)?;
        Err(PinsError::Unsupported)
    }

    async fn active(&self, _frame: &Delivery, number: u8) -> Result<bool, PinsError> {
        let input = self.issued(number)?;
        let slot = self.driver.slot(&input.line.controller);
        let gpio = slot.lock(&self.driver).await?;
        let high = gpio
            .as_ref()
            .ok_or(PinsError::Failed)?
            .level(input.line.block)
            .await?;
        Ok(input.line.active(high))
    }

    fn set(&self, _frame: &Delivery, _line: u8, _active: bool) -> Result<(), PinsError> {
        Err(PinsError::NotIssued)
    }

    async fn irq(
        &self,
        _frame: &Delivery,
        number: u8,
    ) -> impl IntoWireValue<Delivery, Read = Result<ipc::wire::Cap<syscall::kind::Signal>, PinsError>>
    {
        self.signal(number).await
    }

    async fn ack(&self, _frame: &Delivery, number: u8) -> Result<(), PinsError> {
        let input = self.issued(number)?;
        let irq = input
            .irq
            .lock()
            .await
            .as_ref()
            .cloned()
            .ok_or(PinsError::NotIssued)?;

        irq.ack().await.map_err(irq_refused)
    }
}
