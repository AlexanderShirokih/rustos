/// Кодировка прав доступа к user-памяти в syscall ABI.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[repr(u8)]
pub enum UserMemFlags {
    ReadWrite = 0,
    ReadOnly = 1,
    ReadExecute = 2,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct InvalidUserMemFlags;

impl UserMemFlags {
    pub const fn from_raw(raw: u64) -> Result<Self, InvalidUserMemFlags> {
        match raw {
            0 => Ok(Self::ReadWrite),
            1 => Ok(Self::ReadOnly),
            2 => Ok(Self::ReadExecute),
            _ => Err(InvalidUserMemFlags),
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
        assert_eq!(UserMemFlags::from_raw(0), Ok(UserMemFlags::ReadWrite));
        assert_eq!(UserMemFlags::from_raw(1), Ok(UserMemFlags::ReadOnly));
        assert_eq!(UserMemFlags::from_raw(2), Ok(UserMemFlags::ReadExecute));
    }

    #[test]
    fn from_raw_unknown_is_invalid() {
        assert_eq!(UserMemFlags::from_raw(3), Err(InvalidUserMemFlags));
        assert_eq!(UserMemFlags::from_raw(u64::MAX), Err(InvalidUserMemFlags));
    }

    #[test]
    fn raw_round_trips_with_from_raw() {
        for f in [
            UserMemFlags::ReadWrite,
            UserMemFlags::ReadOnly,
            UserMemFlags::ReadExecute,
        ] {
            assert_eq!(UserMemFlags::from_raw(f.raw()), Ok(f));
        }
    }
}
