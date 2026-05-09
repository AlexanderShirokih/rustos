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
