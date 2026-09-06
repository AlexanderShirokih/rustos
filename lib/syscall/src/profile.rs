/// Кодировка профиля работы с памятью в syscall ABI.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[repr(u8)]
pub enum MemoryProfile {
    /// Кэшируемая память с обратной записью.
    Cached = 0,
    /// Некэшируемая память с объединением записей в пакеты, порядок между ними
    /// восстанавливается барьерами.
    WriteCombining = 1,
    /// Регистры устройства, доступ к которым не объединяется и не переупорядочивается.
    Device = 2,
}

/// Значение вне множества профилей.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct InvalidMemoryProfile;

impl MemoryProfile {
    pub const fn from_raw(raw: u64) -> Result<Self, InvalidMemoryProfile> {
        match raw {
            0 => Ok(Self::Cached),
            1 => Ok(Self::WriteCombining),
            2 => Ok(Self::Device),
            _ => Err(InvalidMemoryProfile),
        }
    }

    pub const fn raw(self) -> u64 {
        self as u8 as u64
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn from_raw_known() {
        assert_eq!(MemoryProfile::from_raw(0), Ok(MemoryProfile::Cached));
        assert_eq!(
            MemoryProfile::from_raw(1),
            Ok(MemoryProfile::WriteCombining)
        );
        assert_eq!(MemoryProfile::from_raw(2), Ok(MemoryProfile::Device));
    }

    #[test]
    fn from_raw_unknown_is_invalid() {
        assert_eq!(MemoryProfile::from_raw(3), Err(InvalidMemoryProfile));
        assert_eq!(MemoryProfile::from_raw(u64::MAX), Err(InvalidMemoryProfile));
    }

    #[test]
    fn raw_round_trips_with_from_raw() {
        for profile in [
            MemoryProfile::Cached,
            MemoryProfile::WriteCombining,
            MemoryProfile::Device,
        ] {
            assert_eq!(MemoryProfile::from_raw(profile.raw()), Ok(profile));
        }
    }
}
