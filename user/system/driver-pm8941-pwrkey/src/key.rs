//! Кнопки блока PON и правила `drivers/input/misc/pm8941-pwrkey.c` Linux: выбор клавиши,
//! кодирование антидребезга, окно программного антидребезга и переходы состояния клавиши.

use input_abi::Usage;

pub(crate) const DEBOUNCE_DEFAULT_US: u32 = 15_625;

const DEBOUNCE_MAX_US: u32 = 2_000_000;
const US_PER_SEC: u64 = 1_000_000;

const WINDOW_MAX_NS: u64 = 2_000_000_000;

const SUBTYPE_GEN2_PRIMARY: u8 = 4;

/// Кнопка питания.
pub const PWRKEY: Key = Key {
    compatible: "qcom,pm8941-pwrkey",
    name: "pm8941_pwrkey",
    status_bit: 1 << 0,
    pull_up_bit: 1 << 1,
};

/// Кнопка сброса.
pub const RESIN: Key = Key {
    compatible: "qcom,pm8941-resin",
    name: "pm8941_resin",
    status_bit: 1 << 1,
    pull_up_bit: 1 << 0,
};

/// Кнопка блока PON: совместимость узла и её биты в регистрах PON.
#[derive(Debug)]
pub struct Key {
    /// Совместимость узла кнопки.
    pub compatible: &'static str,
    /// Имя устройства ввода.
    pub name: &'static str,
    pub(crate) status_bit: u8,
    /// Бит подтяжки кнопки.
    pub pull_up_bit: u8,
}

/// Поколение блока PON, которое задаёт регистр подтипа.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Generation {
    /// Подтипы ниже `GEN2_PRIMARY`.
    First,
    /// Подтип `GEN2_PRIMARY` и выше.
    Second,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Transition {
    None,
    Press,
    Release,
    PressAndRelease,
}

impl Generation {
    /// Поколение по значению регистра подтипа.
    pub fn of(subtype: u8) -> Self {
        if subtype >= SUBTYPE_GEN2_PRIMARY {
            Self::Second
        } else {
            Self::First
        }
    }

    pub(crate) fn mask(self) -> u8 {
        match self {
            Self::First => 0x7,
            Self::Second => 0xF,
        }
    }

    fn shift(self) -> u32 {
        match self {
            Self::First => 6,
            Self::Second => 14,
        }
    }

    /// Поле антидребезга для `us` микросекунд: `ilog2((us << shift) / 1 000 000)`. Время
    /// короче наименьшего шага даёт 0. Время 0 и больше 2 000 000 мкс дают `None`.
    pub fn debounce_field(self, us: u32) -> Option<u8> {
        if us == 0 || us > DEBOUNCE_MAX_US {
            return None;
        }

        let steps = (u64::from(us) << self.shift()) / US_PER_SEC;
        let field = steps.checked_ilog2().unwrap_or(0);
        // В допустимом диапазоне времени поле не превышает маску поколения.
        u8::try_from(field).ok()
    }

    /// Окно программного антидребезга в наносекундах для поля антидребезга `field`:
    /// `2 с / 2^(m - field)`, где `m` - маска поля.
    pub fn debounce_window(self, field: u8) -> u64 {
        let mask = self.mask();
        WINDOW_MAX_NS >> (mask - (field & mask))
    }
}

pub(crate) fn transition(pressed: bool, set: bool) -> Transition {
    match (pressed, set) {
        (false, true) => Transition::Press,
        (true, false) => Transition::Release,
        (false, false) => Transition::PressAndRelease,
        (true, true) => Transition::None,
    }
}

/// Клавиша по коду `linux,code` узла. Узел без кода получает `Power`, как в Linux. Код без
/// перевода возвращается ошибкой.
pub fn usage(code: Option<u32>) -> Result<Usage, u32> {
    code.map_or(Ok(Usage::POWER), |code| {
        Usage::from_linux_key(code).ok_or(code)
    })
}
