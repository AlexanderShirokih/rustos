use memory::MemFlags;

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

    pub const fn to_mem_flags(self) -> MemFlags {
        match self {
            Self::ReadWrite => MemFlags::user_rw(),
            Self::ReadOnly => MemFlags::user_ro(),
            Self::ReadExecute => MemFlags::user_rx(),
        }
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

    fn user_rwx(flags: MemFlags) -> (bool, bool, bool) {
        use memory::mem_flags::{AccessMode, Executable};
        match flags {
            MemFlags::Private(o) => (
                matches!(o.user.access, AccessMode::Readonly | AccessMode::Writable),
                matches!(o.user.access, AccessMode::Writable),
                matches!(o.user.executable, Executable::Allowed),
            ),
            MemFlags::Device(_) => (false, false, false),
        }
    }

    #[test]
    fn map_to_matching_mem_flags() {
        assert_eq!(user_rwx(UserMemFlags::ReadWrite.to_mem_flags()), (true, true, false));
        assert_eq!(user_rwx(UserMemFlags::ReadOnly.to_mem_flags()), (true, false, false));
        assert_eq!(user_rwx(UserMemFlags::ReadExecute.to_mem_flags()), (true, false, true));
    }
}
