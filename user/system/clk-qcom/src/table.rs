//! Описание тактов в таблице SoC: ветвь, её включение и генератор.

/// Такт контроллера: номер в записях `clocks` и его ветвь.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Clock {
    /// Номер такта в записях `clocks` узлов.
    pub index: u32,
    /// Ветвь такта.
    pub branch: Branch,
}

/// Ветвь такта: способ включения, регистр состояния и генератор.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Branch {
    /// Способ включения ветви.
    pub enable: Enable,
    /// Смещение регистра состояния ветви.
    pub halt: usize,
    /// Генератор частоты ветви.
    pub generator: Option<Generator>,
}

/// Способ включения ветви.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Enable {
    /// Бит 0 регистра `CBCR` по смещению.
    Branch(usize),
    /// Маска бита голоса процессора приложений.
    Vote(u32),
}

/// Генератор частоты (RCG): регистр `CMD` и строки таблицы частот.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Generator {
    /// Смещение регистра `CMD`, регистр `CFG` следует за ним.
    pub command: usize,
    /// Строки таблицы частот.
    pub frequencies: &'static [Frequency],
}

/// Строка таблицы частот генератора.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Frequency {
    /// Частота в герцах.
    pub hz: u64,
    /// Номер источника в поле выбора источника `CFG`.
    pub source: u32,
    /// Значение поля делителя `CFG`: `2h − 1` для делителя `h`.
    pub divider: u32,
}
