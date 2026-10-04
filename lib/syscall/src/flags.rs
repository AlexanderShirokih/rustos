/// Кодировка прав доступа к user-памяти в syscall ABI. Множество замкнуто, и ядро
/// отвергает значение вне него как `InvalidArgument`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[repr(u8)]
pub enum UserMemFlags {
    ReadWrite = 0,
    ReadOnly = 1,
    ReadExecute = 2,
}

/// Значение вне множества флагов доступа.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct InvalidUserMemFlags;

/// Бит аргумента `flags` у `MemoryAllocate`, который объединяется с [`UserMemFlags`]. Ядро
/// оставляет под маппингом одну неотображённую страницу и не выдаёт её другим маппингам.
/// Прочие memory-вызовы отвергают бит как `InvalidArgument`.
pub const MEMORY_ALLOCATE_GUARD_PAGE: u64 = 1 << 8;

/// Бит аргумента `region_flags` у `MemoryCreateVirtual`. С этим битом ядро выделяет регион
/// из физически смежных страниц. `MemoryRegionInspect` возвращает в `base_pa` адрес первой
/// страницы региона. `MemoryCreateVirtual` отвергает прочие биты как `InvalidArgument`.
pub const MEMORY_REGION_CONTIGUOUS: u64 = 1 << 0;

/// Бит аргумента `region_flags` у `MemorySlice`. С этим битом ядро создаёт срез профиля
/// [`MemoryProfile::WriteCombining`](crate::MemoryProfile::WriteCombining) у региона профиля
/// `Device` либо `WriteCombining`. Для `Cached`-региона вызов возвращает `InvalidArgument`.
/// `MemorySlice` отвергает прочие биты как `InvalidArgument`.
pub const MEMORY_REGION_WRITE_COMBINING: u64 = 1 << 1;

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
