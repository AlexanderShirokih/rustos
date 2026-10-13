//! Устройства за контроллерами QUP: поиск устройства по пути, подготовка контроллера по первому
//! открытию и занятость устройств сессиями.

use alloc::vec::Vec;

use i2c_abi::{I2cError, TransferBuf, TransferError};
use mmio::Registers;
use platform_abi::{Description, DescriptionError, DeviceClaims, DeviceId};

use crate::controller::{CORE_HZ, Controller, Line, Operation, Refusal, clock_control};

const COMPATIBLE: &str = "qcom,i2c-qup-v2.2.1";
const BUS_FREQUENCY: &str = "clock-frequency";
const DEFAULT_BUS_HZ: u32 = 100_000;
const SEVEN_BIT_ADDRESS_MAX: u32 = 0x7f;

/// Ресурсы контроллера, которые шина берёт у поставщиков при подготовке. Узел контроллера
/// называет `controller`.
pub trait Resources {
    /// Окно регистров контроллера.
    type Window: Registers<()>;
    /// Линия прерывания контроллера.
    type Line: Line;
    /// Отказ поставщика ресурса.
    type Error;

    /// Применяет состояние `default` выводов узла контроллера, включает его такты, задаёт
    /// такту `core` частоту `core_hz` и отображает окно регистров.
    fn enable(
        &mut self,
        description: &Description<'_>,
        controller: &DeviceId,
        core_hz: u32,
    ) -> Result<Self::Window, Self::Error>;

    /// Выдаёт линию прерывания контроллера.
    fn line(&mut self, controller: &DeviceId) -> Result<Self::Line, Self::Error>;

    /// Сообщает об отказе подготовки контроллера. Отказ закреплён до конца процесса.
    fn refused(&mut self, controller: &DeviceId, failure: &Failure<Self::Error>);
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

/// Устройство сессии: подготовленный контроллер, его линия и адрес устройства.
pub struct Device<'b, W, L> {
    controller: &'b Controller<W>,
    line: &'b L,
    address: u8,
}

/// Устройства за контроллерами QUP описания, их занятость сессиями и состояние подготовки
/// контроллеров.
pub struct Bus<'d, R: Resources> {
    description: &'d Description<'d>,
    resources: R,
    controllers: Vec<(DeviceId, Option<Prepared<R>>)>,
    claims: DeviceClaims,
}

type Prepared<R> = (Controller<<R as Resources>::Window>, <R as Resources>::Line);

impl<W: Registers<()>, L: Line> Device<'_, W, L> {
    /// Адрес устройства на шине.
    pub fn address(&self) -> u8 {
        self.address
    }

    /// Выполняет передачу `operation` устройству, как [`Controller::transfer`].
    pub fn transfer(&self, operation: Operation<'_>) -> Result<TransferBuf, TransferError> {
        self.controller.transfer(self.line, self.address, operation)
    }
}

impl<'d, R: Resources> Bus<'d, R> {
    /// Шина устройств описания `description` без подготовленных контроллеров и занятых
    /// устройств.
    pub fn new(description: &'d Description<'d>, resources: R) -> Self {
        Self {
            description,
            resources,
            controllers: Vec::new(),
            claims: DeviceClaims::new(),
        }
    }

    /// Проверяет, что устройство с узлом `device` можно открыть, и готовит его контроллер по
    /// первому открытию. Устройство занимает [`claim`](Self::claim).
    ///
    /// Отказы:
    /// - `NoDevice` - путь не называет устройство за контроллером QUP либо подготовка его
    ///   контроллера отказала сейчас или раньше;
    /// - `Busy` - устройство занято сессией;
    /// - `Unsupported` - адрес выше 0x7f либо с флагами.
    pub fn prepare(&mut self, device: &DeviceId) -> Result<(), I2cError> {
        let (controller, _) = locate(self.description, device)?;
        if self.claims.holds_device(device) {
            return Err(I2cError::Busy);
        }

        if let Some((_, prepared)) = self.controllers.iter().find(|(id, _)| *id == controller) {
            return prepared.as_ref().map(drop).ok_or(I2cError::NoDevice);
        }

        let prepared = self
            .ready(&controller)
            .inspect_err(|failure| self.resources.refused(&controller, failure))
            .ok();
        let result = prepared.as_ref().map(drop).ok_or(I2cError::NoDevice);
        self.controllers.push((controller, prepared));
        result
    }

    /// Занимает устройство `device` сессией со значком `badge`.
    pub fn claim(&mut self, badge: u64, device: DeviceId) {
        self.claims.claim(badge, device);
    }

    /// Освобождает устройство сессии со значком `badge`.
    pub fn release(&mut self, badge: u64) {
        self.claims.release(badge);
    }

    /// Устройство сессии со значком `badge` либо `None`, если сессия не держит устройство.
    pub fn device(&self, badge: u64) -> Option<Device<'_, R::Window, R::Line>> {
        let device = self.claims.devices(badge).next()?;
        let (controller, address) = locate(self.description, device).ok()?;
        let (_, prepared) = self.controllers.iter().find(|(id, _)| *id == controller)?;
        let (controller, line) = prepared.as_ref()?;
        Some(Device {
            controller,
            line,
            address,
        })
    }

    fn ready(&mut self, controller: &DeviceId) -> Result<Prepared<R>, Failure<R::Error>> {
        let bus_hz = self
            .description
            .node(controller)
            .map(|node| node.u32(BUS_FREQUENCY))
            .transpose()
            .map_err(Failure::Description)?
            .flatten()
            .unwrap_or(DEFAULT_BUS_HZ);
        let clock_control = clock_control(bus_hz).ok_or(Failure::Frequency(bus_hz))?;

        let window = self
            .resources
            .enable(self.description, controller, CORE_HZ)
            .map_err(Failure::Resources)?;
        let registers = Controller::new(window, clock_control).map_err(Failure::Controller)?;
        let line = self
            .resources
            .line(controller)
            .map_err(Failure::Resources)?;
        Ok((registers, line))
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
