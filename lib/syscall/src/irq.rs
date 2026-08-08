//! Триггер линии прерывания в syscall-ABI.

/// Как контроллер распознаёт активность линии.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[repr(u32)]
pub enum IrqTrigger {
    /// По фронту.
    Edge = 1,
    /// По уровню.
    Level = 2,
}

impl IrqTrigger {
    /// Стабильный численный код варианта (часть ABI).
    pub const fn code(self) -> u32 {
        self as u32
    }

    /// Восстанавливает вариант по его коду. `None` на неизвестном коде.
    pub const fn from_code(code: u32) -> Option<Self> {
        match code {
            1 => Some(Self::Edge),
            2 => Some(Self::Level),
            _ => None,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::IrqTrigger;

    #[test]
    fn code_and_from_code_round_trip() {
        for trigger in [IrqTrigger::Edge, IrqTrigger::Level] {
            assert_eq!(IrqTrigger::from_code(trigger.code()), Some(trigger));
        }
        assert_eq!(IrqTrigger::from_code(0), None);
        assert_eq!(IrqTrigger::from_code(3), None);
    }
}
