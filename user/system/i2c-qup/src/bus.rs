//! Устройства за контроллерами QUP: подготовка контроллера по первому открытию и занятость
//! устройств сессиями. Каждый контроллер сериализует свои передачи независимо от остальных.

use alloc::{rc::Rc, vec::Vec};
use core::cell::RefCell;

use executor::Mutex;
use i2c_abi::{I2cError, TransferBuf, TransferError};
use mmio::Registers;
use platform_abi::{Description, DescriptionError, DeviceClaims, DeviceId};

use crate::controller::{CORE_HZ, Controller, Line, Operation, Refusal, clock_control};

const COMPATIBLE: &str = "qcom,i2c-qup-v2.2.1";
const BUS_FREQUENCY: &str = "clock-frequency";
const DEFAULT_BUS_HZ: u32 = 100_000;
const SEVEN_BIT_ADDRESS_MAX: u32 = 0x7f;

/// Ресурсы контроллера, которые шина берёт у поставщиков при подготовке.
#[allow(async_fn_in_trait)]
pub trait Resources {
    /// Окно регистров контроллера.
    type Window: Registers<()>;
    /// Линия прерывания контроллера.
    type Line: Line;
    /// Отказ поставщика ресурса.
    type Error;

    /// Применяет состояние `default` выводов узла, включает его такты, задаёт такту `core`
    /// частоту `core_hz` и отображает окно регистров.
    async fn enable(
        &self,
        description: &Description<'_>,
        controller: &DeviceId,
        core_hz: u32,
    ) -> Result<Self::Window, Self::Error>;

    /// Выдаёт линию прерывания контроллера.
    async fn line(&self, controller: &DeviceId) -> Result<Self::Line, Self::Error>;

    /// Сообщает об отказе подготовки, закреплённом до конца процесса.
    fn refused(&self, controller: &DeviceId, failure: &Failure<Self::Error>);
}

/// Причина отказа подготовки контроллера.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Failure<E> {
    /// `clock-frequency` узла не читается.
    Description(DescriptionError),
    /// Частота шины равна 0, выше 400 кГц либо ниже, чем допускает делитель `QUP_I2C_CLK_CTL`.
    Frequency(u32),
    /// Поставщик ресурса отказал.
    Resources(E),
    /// Контроллер не готов к работе.
    Controller(Refusal),
}

/// Устройство сессии, удерживающее ресурсы подготовленного контроллера.
pub struct Device<W, L> {
    prepared: Rc<Prepared<W, L>>,
    address: u8,
}

/// Занятость устройств сессиями и состояние подготовки контроллеров.
pub struct Bus<R: Resources> {
    resources: R,
    controllers: RefCell<Vec<(DeviceId, Rc<Slot<R>>)>>,
    claims: RefCell<DeviceClaims>,
}

struct Prepared<W, L> {
    controller: Mutex<Controller<W>>,
    line: L,
}

struct Slot<R: Resources> {
    preparing: Mutex<()>,
    state: RefCell<Preparation<R>>,
}

enum Preparation<R: Resources> {
    New,
    Ready(Rc<Prepared<R::Window, R::Line>>),
    Refused,
}

impl<W, L> Clone for Device<W, L> {
    fn clone(&self) -> Self {
        Self {
            prepared: self.prepared.clone(),
            address: self.address,
        }
    }
}

impl<W: Registers<()>, L: Line> Device<W, L> {
    /// Адрес устройства на шине.
    pub fn address(&self) -> u8 {
        self.address
    }

    /// Выполняет передачу с исключительным доступом к контроллеру до её завершения или
    /// отмены. Отмена сбрасывает контроллер до снятия mutex.
    pub async fn transfer(&self, operation: Operation<'_>) -> Result<TransferBuf, TransferError> {
        let controller = self.prepared.controller.lock().await;
        controller
            .transfer(&self.prepared.line, self.address, operation)
            .await
    }
}

impl<R: Resources> Bus<R> {
    /// Шина без подготовленных контроллеров и занятых устройств.
    pub fn new(resources: R) -> Self {
        Self {
            resources,
            controllers: RefCell::new(Vec::new()),
            claims: RefCell::new(DeviceClaims::new()),
        }
    }

    /// Проверяет устройство и готовит его контроллер по первому открытию. Устройство занимает
    /// [`claim`](Self::claim) без ожиданий после успешного возврата.
    ///
    /// Отказы:
    /// - `NoDevice` - устройство или контроллер недоступны;
    /// - `Busy` - устройство занято;
    /// - `Unsupported` - адрес выше 0x7f либо с флагами.
    pub async fn prepare(
        &self,
        description: &Description<'_>,
        device: &DeviceId,
    ) -> Result<(), I2cError> {
        let (controller, _) = locate(description, device)?;

        if self.claims.borrow().holds_device(device) {
            return Err(I2cError::Busy);
        }

        let slot = {
            let mut controllers = self.controllers.borrow_mut();
            if let Some((_, slot)) = controllers.iter().find(|(id, _)| *id == controller) {
                slot.clone()
            } else {
                let slot = Rc::new(Slot {
                    preparing: Mutex::new(()),
                    state: RefCell::new(Preparation::New),
                });
                controllers.push((controller.clone(), slot.clone()));
                slot
            }
        };

        let _preparing = slot.preparing.lock().await;

        let unprepared = matches!(*slot.state.borrow(), Preparation::New);
        if unprepared {
            let prepared = self.ready(description, &controller).await;
            *slot.state.borrow_mut() = match prepared {
                Ok(prepared) => Preparation::Ready(Rc::new(prepared)),
                Err(failure) => {
                    self.resources.refused(&controller, &failure);
                    Preparation::Refused
                }
            };
        }

        if matches!(*slot.state.borrow(), Preparation::Refused) {
            return Err(I2cError::NoDevice);
        }

        if self.claims.borrow().holds_device(device) {
            return Err(I2cError::Busy);
        }

        Ok(())
    }

    /// Занимает устройство `device` сессией со значком `badge`.
    pub fn claim(&self, badge: u64, device: DeviceId) {
        self.claims.borrow_mut().claim(badge, device);
    }

    /// Освобождает устройство сессии со значком `badge`.
    pub fn release(&self, badge: u64) {
        self.claims.borrow_mut().release(badge);
    }

    /// Устройство сессии либо `None`, если сессия не держит подготовленное устройство.
    pub fn device(
        &self,
        description: &Description<'_>,
        badge: u64,
    ) -> Option<Device<R::Window, R::Line>> {
        let claims = self.claims.borrow();
        let device = claims.devices(badge).next()?;
        let (controller, address) = locate(description, device).ok()?;

        let controllers = self.controllers.borrow();
        let (_, slot) = controllers.iter().find(|(id, _)| *id == controller)?;
        let state = slot.state.borrow();
        let Preparation::Ready(prepared) = &*state else {
            return None;
        };

        Some(Device {
            prepared: prepared.clone(),
            address,
        })
    }

    async fn ready(
        &self,
        description: &Description<'_>,
        controller: &DeviceId,
    ) -> Result<Prepared<R::Window, R::Line>, Failure<R::Error>> {
        let bus_hz = description
            .node(controller)
            .map(|node| node.u32(BUS_FREQUENCY))
            .transpose()
            .map_err(Failure::Description)?
            .flatten()
            .unwrap_or(DEFAULT_BUS_HZ);
        let clock_control = clock_control(bus_hz).ok_or(Failure::Frequency(bus_hz))?;

        let window = self
            .resources
            .enable(description, controller, CORE_HZ)
            .await
            .map_err(Failure::Resources)?;

        let registers = Controller::new(window, clock_control).map_err(Failure::Controller)?;
        let line = self
            .resources
            .line(controller)
            .await
            .map_err(Failure::Resources)?;

        Ok(Prepared {
            controller: Mutex::new(registers),
            line,
        })
    }
}

fn locate(description: &Description<'_>, device: &DeviceId) -> Result<(DeviceId, u8), I2cError> {
    // `Description::node` выдаёт узел только при доступных предках, и статус родителя уже
    // проверен.
    let node = description.node(device).ok_or(I2cError::NoDevice)?;
    let parent = description
        .ancestors(device)
        .next()
        .ok_or(I2cError::NoDevice)?;
    let compatible = parent
        .node
        .strings("compatible")
        .ok()
        .flatten()
        .is_some_and(|mut names| names.any(|name| name == COMPATIBLE));
    if !compatible {
        return Err(I2cError::NoDevice);
    }

    let cell = node
        .cells("reg")
        .ok()
        .flatten()
        .and_then(|mut cells| cells.next())
        .ok_or(I2cError::NoDevice)?;
    if cell > SEVEN_BIT_ADDRESS_MAX {
        return Err(I2cError::Unsupported);
    }
    let address = u8::try_from(cell).map_err(|_| I2cError::Unsupported)?;
    Ok((parent.id, address))
}
