//! Применение состояния `default` узла, линии выводов контроллера TLMM и их прерывания.

use alloc::vec::Vec;

use mmio::Registers;
use pins_abi::{
    Claims, GPIO_ACTIVE_LOW, Pin, PinsError, check_state, default_states, pin_names, pin_number,
    sets,
};
use platform_abi::{Description, DeviceId};

use crate::{
    registers::{PinGroup, Tile},
    settings::{self, GPIO_FUNCTION_NUMBER, Setting},
};

const PIN_STRIDE: usize = 0x1000;
const CTL: usize = 0x0;
const IO: usize = 0x4;
const INTR_CFG: usize = 0x8;
const INTR_STATUS: usize = 0xc;

const PULL_SHIFT: u32 = 0;
const PULL_MASK: u32 = 0x3;
const FUNCTION_SHIFT: u32 = 2;
const FUNCTION_MASK: u32 = 0xF;
const DRIVE_SHIFT: u32 = 6;
const DRIVE_MASK: u32 = 0x7;
const OE: u32 = 1 << 9;

const IO_IN: u32 = 1 << 0;
const IO_OUT: u32 = 1 << 1;

const INTR_ENABLE: u32 = 1 << 0;
const INTR_POLARITY: u32 = 1 << 1;
const DETECTION_SHIFT: u32 = 2;
const DETECTION_MASK: u32 = 0x3;
const RAW_STATUS_EN: u32 = 1 << 4;
const TARGET_SHIFT: u32 = 5;
const TARGET_MASK: u32 = 0x7;
const TARGET_APPS: u32 = 3;

const INTR_LATCHED: u32 = 1 << 0;

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

/// Линия записи `gpios` либо `interrupts-extended` под контроллером TLMM.
#[derive(Clone)]
pub struct Line {
    /// Вывод линии.
    pub pin: Pin,
    tile: Tile,
    active_low: bool,
    trigger: Trigger,
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum Trigger {
    Rising,
    Falling,
    Both,
    High,
    Low,
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

    /// Линия `index` с именем `name` узла `node` для сессии узла `session`. Ячейки
    /// записи - номер вывода от 0 и флаги. Прерывание линии срабатывает при подъёме и спаде
    /// уровня. Регистры не читаются и не пишутся.
    ///
    /// Отказы в порядке проверки:
    /// - [`PinsError::NoDevice`] - `node` не узел сессии и не его потомок либо в описании нет
    ///   записи линии;
    /// - [`PinsError::Unsupported`] - запись называет другой контроллер, число ячеек кроме двух
    ///   либо флаги кроме `GPIO_ACTIVE_LOW`;
    /// - [`PinsError::Denied`] - вывод закрыт;
    /// - [`PinsError::Unsupported`] - вывод вне таблицы.
    pub fn line(
        &self,
        description: &Description<'_>,
        session: &DeviceId,
        node: &DeviceId,
        name: &str,
        index: u8,
    ) -> Result<Line, PinsError> {
        let record =
            pins_abi::line(description, session, node, name, index).ok_or(PinsError::NoDevice)?;
        let [pin, flags] = record.cells[..] else {
            return Err(PinsError::Unsupported);
        };
        if record.controller != self.id || flags & !GPIO_ACTIVE_LOW != 0 {
            return Err(PinsError::Unsupported);
        }

        self.pin_line(pin, flags & GPIO_ACTIVE_LOW != 0, Trigger::Both)
    }

    /// Линия записи `index` свойства `interrupts-extended` узла `node` для сессии узла
    /// `session`. Ячейки записи - номер вывода от 0 и условие срабатывания `IRQ_TYPE_*`.
    /// Регистры не читаются и не пишутся.
    ///
    /// Отказы в порядке проверки:
    /// - [`PinsError::NoDevice`] - `node` не узел сессии и не его потомок либо в описании нет
    ///   записи;
    /// - [`PinsError::Unsupported`] - запись называет другой контроллер, число ячеек кроме двух
    ///   либо условие кроме подъёма, спада, обоих фронтов, высокого и низкого уровня;
    /// - [`PinsError::Denied`] - вывод закрыт;
    /// - [`PinsError::Unsupported`] - вывод вне таблицы.
    pub fn interrupt(
        &self,
        description: &Description<'_>,
        session: &DeviceId,
        node: &DeviceId,
        index: u8,
    ) -> Result<Line, PinsError> {
        let record =
            pins_abi::interrupt(description, session, node, index).ok_or(PinsError::NoDevice)?;
        let [pin, trigger] = record.cells[..] else {
            return Err(PinsError::Unsupported);
        };
        if record.controller != self.id {
            return Err(PinsError::Unsupported);
        }
        let trigger = Trigger::from_cell(trigger).ok_or(PinsError::Unsupported)?;

        self.pin_line(pin, false, trigger)
    }

    /// Переводит вывод линии в функцию `gpio` и снимает разрешение выхода одной записью `CTL`.
    pub fn input(&self, line: &Line) {
        let ctl = self.load(line, CTL);
        self.store(line, CTL, gpio_function(ctl) & !OE);
    }

    /// Записывает в `IO` выходной уровень `active` линии, затем одной записью `CTL` переводит
    /// вывод в функцию `gpio` с разрешением выхода.
    pub fn output(&self, line: &Line, active: bool) {
        self.set(line, active);
        let ctl = self.load(line, CTL);
        self.store(line, CTL, gpio_function(ctl) | OE);
    }

    /// Возвращает `true`, если входной уровень вывода линии - её активный уровень.
    pub fn active(&self, line: &Line) -> bool {
        (self.load(line, IO) & IO_IN != 0) != line.active_low
    }

    /// Выставляет в `IO` высокий выходной уровень вывода линии, если `active` не совпадает с
    /// флагом `GPIO_ACTIVE_LOW` записи, и низкий уровень иначе.
    pub fn set(&self, line: &Line, active: bool) {
        let io = self.load(line, IO);
        let io = if active == line.active_low {
            io & !IO_OUT
        } else {
            io | IO_OUT
        };
        self.store(line, IO, io);
    }

    /// Настраивает прерывание вывода линии по её условию срабатывания с получателем -
    /// процессором приложений, снимает его защёлку и затем разрешает прерывание.
    pub fn enable_irq(&self, line: &Line) {
        let cfg = self.load(line, INTR_CFG);
        let fields = INTR_ENABLE
            | INTR_POLARITY
            | DETECTION_MASK << DETECTION_SHIFT
            | TARGET_MASK << TARGET_SHIFT;
        let cfg = cfg & !fields | TARGET_APPS << TARGET_SHIFT | RAW_STATUS_EN | line.trigger.cfg();

        self.store(line, INTR_CFG, cfg);
        self.store(line, INTR_STATUS, 0);
        self.store(line, INTR_CFG, cfg | INTR_ENABLE);
    }

    /// Возвращает `true`, если прерывание вывода линии разрешено и защёлкнуто. Защёлку
    /// читает только у разрешённого прерывания.
    pub fn irq_fired(&self, line: &Line) -> bool {
        self.load(line, INTR_CFG) & INTR_ENABLE != 0
            && self.load(line, INTR_STATUS) & INTR_LATCHED != 0
    }

    /// Снимает защёлку сработавшего прерывания вывода линии и запрещает его до
    /// [`Tlmm::ack_irq`]. Фронт после снятия защёлки срабатывает после подтверждения, уровень
    /// до подтверждения не защёлкивается.
    pub fn hold_irq(&self, line: &Line) {
        if line.trigger.is_level() {
            self.disable_irq(line);
            self.store(line, INTR_STATUS, 0);
        } else {
            self.store(line, INTR_STATUS, 0);
            self.disable_irq(line);
        }
    }

    /// Снова разрешает прерывание вывода линии и защёлкивание уровня, сохраняя защёлку.
    pub fn ack_irq(&self, line: &Line) {
        let cfg = self.load(line, INTR_CFG);
        self.store(line, INTR_CFG, cfg | RAW_STATUS_EN | INTR_ENABLE);
    }

    /// Запрещает прерывание вывода линии. У прерывания по уровню также запрещает защёлкивание
    /// уровня.
    pub fn disable_irq(&self, line: &Line) {
        let mut cfg = self.load(line, INTR_CFG) & !INTR_ENABLE;
        if line.trigger.is_level() {
            cfg &= !RAW_STATUS_EN;
        }
        self.store(line, INTR_CFG, cfg);
    }

    fn pin_line(&self, pin: u32, active_low: bool, trigger: Trigger) -> Result<Line, PinsError> {
        let pin = u16::try_from(pin).map_err(|_| PinsError::Unsupported)?;
        if self.is_reserved(pin) {
            return Err(PinsError::Denied);
        }
        let group = self
            .groups
            .get(usize::from(pin))
            .ok_or(PinsError::Unsupported)?;

        Ok(Line {
            pin: Pin {
                controller: self.id.clone(),
                number: pin,
            },
            tile: group.tile,
            active_low,
            trigger,
        })
    }

    fn load(&self, line: &Line, register: usize) -> u32 {
        let base = usize::from(line.pin.number) * PIN_STRIDE;
        self.registers.load(line.tile, base + register)
    }

    fn store(&self, line: &Line, register: usize, value: u32) {
        let base = usize::from(line.pin.number) * PIN_STRIDE;
        self.registers.store(line.tile, base + register, value);
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

impl Trigger {
    const fn from_cell(cell: u32) -> Option<Self> {
        match cell {
            1 => Some(Self::Rising),
            2 => Some(Self::Falling),
            3 => Some(Self::Both),
            4 => Some(Self::High),
            8 => Some(Self::Low),
            _ => None,
        }
    }

    const fn cfg(self) -> u32 {
        match self {
            Self::Rising => 1 << DETECTION_SHIFT | INTR_POLARITY,
            Self::Falling => 2 << DETECTION_SHIFT | INTR_POLARITY,
            Self::Both => 3 << DETECTION_SHIFT | INTR_POLARITY,
            Self::High => INTR_POLARITY,
            Self::Low => 0,
        }
    }

    const fn is_level(self) -> bool {
        matches!(self, Self::High | Self::Low)
    }
}

fn gpio_function(ctl: u32) -> u32 {
    with_field(
        ctl,
        FUNCTION_SHIFT,
        FUNCTION_MASK,
        Some(GPIO_FUNCTION_NUMBER),
    )
}

fn with_field(register: u32, shift: u32, mask: u32, value: Option<u32>) -> u32 {
    match value {
        Some(value) => register & !(mask << shift) | (value & mask) << shift,
        None => register,
    }
}
