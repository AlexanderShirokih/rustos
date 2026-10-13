//! Применение состояния `default` узла к выводам контроллера TLMM.

use alloc::vec::Vec;

use mmio::Registers;
use pins_abi::{Claims, Pin, PinsError, check_state, default_states, pin_names, pin_number, sets};
use platform_abi::{Description, DeviceId};

use crate::{
    registers::{PinGroup, Tile},
    settings::{self, GPIO_FUNCTION_NUMBER, Setting},
};

const PIN_STRIDE: usize = 0x1000;
const CTL: usize = 0x0;
const IO: usize = 0x4;

const PULL_SHIFT: u32 = 0;
const PULL_MASK: u32 = 0x3;
const FUNCTION_SHIFT: u32 = 2;
const FUNCTION_MASK: u32 = 0xF;
const DRIVE_SHIFT: u32 = 6;
const DRIVE_MASK: u32 = 0x7;
const OE: u32 = 1 << 9;

const IO_IN: u32 = 1 << 0;
const IO_OUT: u32 = 1 << 1;

const RESERVED_RANGES: &str = "gpio-reserved-ranges";

/// Контроллер TLMM: регистры его плиток, таблица выводов SoC и выводы, закрытые для этой
/// среды исполнения.
pub struct Tlmm<R> {
    registers: R,
    groups: &'static [PinGroup],
    id: DeviceId,
    reserved: Vec<[u32; 2]>,
}

/// Отказ в создании [`Tlmm`].
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Refusal {
    /// Узла контроллера нет в описании, он недоступен либо значение `gpio-reserved-ranges`
    /// повреждено.
    Node,
    /// Окно плитки меньше регистров её выводов.
    Window(Tile),
}

/// Проверенные записи состояния `default` узла, которые [`Tlmm::apply`] пишет в регистры.
pub struct Plan {
    device: DeviceId,
    settings: Vec<Setting>,
    pins: Vec<Pin>,
}

impl<R: Registers<Tile>> Tlmm<R> {
    /// Контроллер узла `id` описания `description` над регистрами `registers` с таблицей
    /// выводов `groups`. Контроллер закрывает выводы из `gpio-reserved-ranges` узла.
    ///
    /// Отказы:
    /// - [`Refusal::Node`] - в описании нет узла, он недоступен либо `gpio-reserved-ranges`
    ///   повреждено;
    /// - [`Refusal::Window`] - окно плитки меньше регистров её выводов.
    pub fn new(
        registers: R,
        groups: &'static [PinGroup],
        description: &Description<'_>,
        id: DeviceId,
    ) -> Result<Self, Refusal> {
        let node = description.node(&id).ok_or(Refusal::Node)?;
        let cells: Vec<u32> = node
            .cells(RESERVED_RANGES)
            .map_err(|_| Refusal::Node)?
            .into_iter()
            .flatten()
            .collect();
        let (reserved, odd_cell) = cells.as_chunks::<2>();
        if !odd_cell.is_empty() {
            return Err(Refusal::Node);
        }

        for (pin, group) in groups.iter().enumerate() {
            if registers.size(group.tile) < (pin + 1) * PIN_STRIDE {
                return Err(Refusal::Window(group.tile));
            }
        }

        Ok(Self {
            registers,
            groups,
            id,
            reserved: reserved.to_vec(),
        })
    }

    /// Проверяет состояние `default` узла с путём `device` и возвращает записи его наборов
    /// под этим контроллером. Узел состояния с `pins` - набор, у узла без `pins` наборами
    /// служат его доступные потомки с `pins`. Регистры не читаются и не пишутся.
    ///
    /// Отказы в порядке проверки:
    /// - [`PinsError::NoDevice`] - узла нет в описании либо он недоступен;
    /// - [`PinsError::Busy`] - узел занят сессией в `claims`;
    /// - [`PinsError::Unsupported`] - ссылка записи состояния не называет узла либо запись
    ///   повреждена;
    /// - [`PinsError::NoDevice`] - состояние не содержит наборов этого контроллера;
    /// - [`PinsError::Denied`] - набор называет закрытый вывод;
    /// - [`PinsError::Unsupported`] - вывод вне таблицы, функция вне таблицы вывода, ток вне
    ///   2..=16 мА либо нечётный, свойство набора вне известных контроллеру, свойство узла
    ///   состояния с потомками-наборами;
    /// - [`PinsError::Busy`] - вывод занят сессией в `claims`.
    pub fn plan(
        &self,
        description: &Description<'_>,
        device: &DeviceId,
        claims: &Claims,
    ) -> Result<Plan, PinsError> {
        description.node(device).ok_or(PinsError::NoDevice)?;
        if claims.holds_device(device) {
            return Err(PinsError::Busy);
        }

        let states: Vec<_> = default_states(description, device.as_str())
            .ok_or(PinsError::Unsupported)?
            .into_iter()
            .filter(|state| state.controller == self.id)
            .map(|state| state.node)
            .collect();
        let sets: Vec<_> = states.iter().copied().flat_map(sets).collect();
        if sets.is_empty() {
            return Err(PinsError::NoDevice);
        }

        let reserved = sets
            .iter()
            .filter_map(pin_names)
            .flatten()
            .filter_map(pin_number)
            .any(|pin| self.is_reserved(pin));
        if reserved {
            return Err(PinsError::Denied);
        }

        for state in &states {
            check_state(state)?;
        }
        let mut parsed = Vec::new();
        for set in &sets {
            settings::parse(set, self.groups, &mut parsed)?;
        }

        let pins: Vec<_> = parsed
            .iter()
            .map(|setting| Pin {
                controller: self.id.clone(),
                number: setting.pin,
            })
            .collect();
        if claims.holds_any(&pins) {
            return Err(PinsError::Busy);
        }

        Ok(Plan {
            device: device.clone(),
            settings: parsed,
            pins,
        })
    }

    /// Записывает поля `CTL` выводов `plan`, каждое изменение - чтением и записью одного
    /// регистра. Перевод в функцию `gpio` вывода с установленным `OE` сначала выставляет в
    /// `IO` выходной уровень, равный входному.
    pub fn apply(&self, plan: &Plan) {
        for setting in &plan.settings {
            let tile = setting.tile;
            let base = usize::from(setting.pin) * PIN_STRIDE;

            let mut ctl = self.registers.load(tile, base + CTL);
            if setting.function == Some(GPIO_FUNCTION_NUMBER) && ctl & OE != 0 {
                let io = self.registers.load(tile, base + IO);
                let level = if io & IO_IN == 0 {
                    io & !IO_OUT
                } else {
                    io | IO_OUT
                };
                if level != io {
                    self.registers.store(tile, base + IO, level);
                }
            }

            ctl = with_field(ctl, PULL_SHIFT, PULL_MASK, setting.pull);
            ctl = with_field(ctl, FUNCTION_SHIFT, FUNCTION_MASK, setting.function);
            ctl = with_field(ctl, DRIVE_SHIFT, DRIVE_MASK, setting.drive);
            self.registers.store(tile, base + CTL, ctl);
        }
    }

    fn is_reserved(&self, pin: u16) -> bool {
        let pin = u32::from(pin);
        self.reserved
            .iter()
            .any(|[start, count]| pin >= *start && pin - start < *count)
    }
}

impl Plan {
    /// Занимает в `claims` узел и выводы состояния сессией со значком `badge`.
    pub fn claim(self, claims: &mut Claims, badge: u64) {
        claims.claim(badge, self.device, self.pins);
    }
}

fn with_field(register: u32, shift: u32, mask: u32, value: Option<u32>) -> u32 {
    match value {
        Some(value) => register & !(mask << shift) | (value & mask) << shift,
        None => register,
    }
}
