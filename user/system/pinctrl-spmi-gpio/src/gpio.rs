//! Регистры блока вывода и их раскладка по подтипу, как `pmic_gpio_config_set` Linux.

use alloc::{collections::BTreeMap, vec::Vec};
use core::ops::RangeInclusive;

use pins_abi::PinsError;
use spmi_abi::DeviceError;

use crate::settings::Setting;

const TYPE: u8 = 0x04;
const SUBTYPE: u8 = 0x05;
const STATUS: u8 = 0x10;
const MODE: u8 = 0x40;
const PULL: u8 = 0x42;
const LV_MV_FUNCTION: u8 = 0x44;
const DRIVE: u8 = 0x45;
const ENABLE: u8 = 0x46;

const GPIO_TYPE: u8 = 0x10;
const SUBTYPES: [u8; 4] = [0x01, 0x05, 0x09, 0x0D];
const LV_MV_SUBTYPES: RangeInclusive<u8> = 0x10..=0x13;

const STATUS_LEVEL: u8 = 1 << 0;
const ENABLED: u8 = 1 << 7;
const PULL_MASK: u8 = 0x07;
const DRIVE_MASK: u8 = 0x03;

const LV_MV_MODE_MASK: u8 = 0x03;
const LV_MV_FUNCTION_MASK: u8 = 0x0F;
const MODE_SHIFT: u32 = 4;
const MODE_MASK: u8 = 0x70;
const FUNCTION_SHIFT: u32 = 1;
const FUNCTION_MASK: u8 = 0x0E;

pub(crate) const MODE_INPUT: u8 = 0;

/// Регистры устройства контроллера за шиной SPMI по 16-битному адресу: старший байт - блок,
/// младший - регистр блока.
pub trait Registers {
    /// Читает регистр `address`.
    fn read(&self, address: u16) -> Result<u8, DeviceError>;

    /// Заменяет биты `mask` регистра `address` битами `value`, остальные биты сохраняются.
    fn update(&self, address: u16, mask: u8, value: u8) -> Result<(), DeviceError>;
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Layout {
    LvMv,
    Shared,
}

/// Выводы одного контроллера над регистрами `R` и подтипы проверенных блоков.
pub struct Gpio<R> {
    registers: R,
    layouts: BTreeMap<u8, Layout>,
}

impl<R: Registers> Gpio<R> {
    /// Контроллер над регистрами `registers`.
    pub fn new(registers: R) -> Self {
        Self {
            registers,
            layouts: BTreeMap::new(),
        }
    }

    /// Регистры контроллера.
    pub fn registers(&self) -> &R {
        &self.registers
    }

    /// Проверяет тип и подтип блока каждой записи, затем пишет записи `settings` и ставит бит
    /// включения их выводов. Поле без значения в записи сохраняется.
    ///
    /// Отказы:
    /// - [`PinsError::Unsupported`] - тип блока кроме 0x10 либо подтип вне известных, регистры
    ///   не пишутся;
    /// - [`PinsError::Denied`] - шина запретила обращение к блоку;
    /// - [`PinsError::Failed`] - шина не выполнила чтение либо запись.
    pub fn apply(&mut self, settings: &[Setting]) -> Result<(), PinsError> {
        let layouts = settings
            .iter()
            .map(|setting| self.layout(setting.block))
            .collect::<Result<Vec<_>, _>>()?;

        for (setting, layout) in settings.iter().zip(layouts) {
            self.mode(setting.block, layout, setting.mode, setting.function)?;
            if let Some(pull) = setting.pull {
                self.update(setting.block, PULL, PULL_MASK, pull)?;
            }
            if let Some(drive) = setting.drive {
                self.update(setting.block, DRIVE, DRIVE_MASK, drive)?;
            }
            self.update(setting.block, ENABLE, ENABLED, ENABLED)?;
        }

        Ok(())
    }

    /// Переводит вывод блока `block` в цифровой вход и ставит бит включения.
    ///
    /// Отказы - как у [`apply`](Self::apply).
    pub fn input(&mut self, block: u8) -> Result<(), PinsError> {
        let layout = self.layout(block)?;
        self.mode(block, layout, Some(MODE_INPUT), None)?;
        self.update(block, ENABLE, ENABLED, ENABLED)
    }

    /// Уровень входа вывода блока `block`: `true` для высокого.
    ///
    /// Отказы:
    /// - [`PinsError::Denied`], [`PinsError::Failed`] - как у [`apply`](Self::apply).
    pub fn level(&self, block: u8) -> Result<bool, PinsError> {
        let status = self
            .registers
            .read(address(block, STATUS))
            .map_err(refused)?;
        Ok(status & STATUS_LEVEL != 0)
    }

    fn layout(&mut self, block: u8) -> Result<Layout, PinsError> {
        if let Some(layout) = self.layouts.get(&block) {
            return Ok(*layout);
        }

        let kind = self.registers.read(address(block, TYPE)).map_err(refused)?;
        if kind != GPIO_TYPE {
            return Err(PinsError::Unsupported);
        }

        let subtype = self
            .registers
            .read(address(block, SUBTYPE))
            .map_err(refused)?;
        let layout = if LV_MV_SUBTYPES.contains(&subtype) {
            Layout::LvMv
        } else if SUBTYPES.contains(&subtype) {
            Layout::Shared
        } else {
            return Err(PinsError::Unsupported);
        };

        self.layouts.insert(block, layout);
        Ok(layout)
    }

    fn mode(
        &self,
        block: u8,
        layout: Layout,
        mode: Option<u8>,
        function: Option<u8>,
    ) -> Result<(), PinsError> {
        match layout {
            Layout::LvMv => {
                if let Some(mode) = mode {
                    self.update(block, MODE, LV_MV_MODE_MASK, mode)?;
                }
                if let Some(function) = function {
                    self.update(block, LV_MV_FUNCTION, LV_MV_FUNCTION_MASK, function)?;
                }
                Ok(())
            }
            Layout::Shared => {
                let (mut mask, mut value) = (0, 0);
                if let Some(mode) = mode {
                    mask |= MODE_MASK;
                    value |= (mode << MODE_SHIFT) & MODE_MASK;
                }
                if let Some(function) = function {
                    mask |= FUNCTION_MASK;
                    value |= (function << FUNCTION_SHIFT) & FUNCTION_MASK;
                }
                if mask == 0 {
                    return Ok(());
                }
                self.update(block, MODE, mask, value)
            }
        }
    }

    fn update(&self, block: u8, register: u8, mask: u8, value: u8) -> Result<(), PinsError> {
        self.registers
            .update(address(block, register), mask, value & mask)
            .map_err(refused)
    }
}

fn address(block: u8, register: u8) -> u16 {
    u16::from_be_bytes([block, register])
}

fn refused(error: DeviceError) -> PinsError {
    match error {
        DeviceError::Denied => PinsError::Denied,
        _ => PinsError::Failed,
    }
}
