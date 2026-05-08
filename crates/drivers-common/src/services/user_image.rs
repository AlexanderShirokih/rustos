//! Архитектурно-независимое описание user-process образа: список сегментов
//! VA -> init-байты + права + параметры user-стека. Используется как input
//! для `SchedulerService::spawn_user_process`.

use memory::{
    MemFlags,
    memory_mapper::MemoryMappingError,
    virtual_address::{PageAlignedVirtualAddress, VirtualAddress},
};

/// Один сегмент user-образа: непрерывный диапазон VA с инициализационными
/// данными и правами.
pub struct UserSegment<'a> {
    /// Базовый VA сегмента (выровнен на 4К).
    pub va_base: PageAlignedVirtualAddress,
    /// Размер замапленной области в байтах (кратно 4К). Хвост, не покрытый
    /// `init_bytes`, остаётся занулённым (BSS).
    pub mapped_size: usize,
    /// Байты, копируемые в начало сегмента. Длина не должна превышать `mapped_size`.
    pub init_bytes: &'a [u8],
    /// Права доступа (типичные конструкторы - `MemFlags::user_rx/ro/rw`).
    pub perms: MemFlags,
}

/// Описание user-process образа. Передаётся в `SchedulerService::spawn_user_process`.
pub struct UserImage<'a> {
    /// Сегменты программы. Не должны пересекаться по VA-диапазонам.
    pub segments: &'a [UserSegment<'a>],
    /// User entry point (попадает в первый PC user-thread'а).
    pub entry: VirtualAddress,
    /// Вершина user-stack (выровнена на 4К). Под
    /// `[user_stack_top - user_stack_size, user_stack_top)` маппится
    /// `user_stack_size / 4096` страниц с правами `user_rw`.
    pub user_stack_top: VirtualAddress,
    /// Размер user-стека (кратен 4К).
    pub user_stack_size: usize,
}

/// Ошибки валидации/загрузки `UserImage`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum UserImageError {
    /// `mapped_size` сегмента или его база не выровнены на 4К.
    MisalignedSegment,
    /// Размер user-стека или его база не выровнены на 4К.
    MisalignedStack,
    /// `init_bytes.len() > mapped_size`.
    InitBytesExceedMapped,
    /// Два сегмента пересекаются по VA-диапазону.
    OverlappingSegments,
    /// `entry` не лежит ни в одном из исполняемых сегментов.
    EntryNotInExecSegment,
    /// User-стек пересекается с одним из сегментов.
    StackOverlapsSegment,
    /// Ошибка маппинга / выделения фреймов.
    Mapping(MemoryMappingError),
}

impl From<MemoryMappingError> for UserImageError {
    fn from(value: MemoryMappingError) -> Self {
        UserImageError::Mapping(value)
    }
}

impl UserImage<'_> {
    /// Базовый VA user-стека (top - size, выровнен на 4К).
    pub fn user_stack_base(&self) -> Result<PageAlignedVirtualAddress, UserImageError> {
        let base = self
            .user_stack_top
            .as_usize()
            .checked_sub(self.user_stack_size)
            .ok_or(UserImageError::MisalignedStack)?;
        PageAlignedVirtualAddress::from_usize(base).ok_or(UserImageError::MisalignedStack)
    }

    /// Проверяет инварианты образа без выполнения каких-либо аллокаций.
    pub fn validate(&self) -> Result<(), UserImageError> {
        const FRAME_SIZE: usize = 4096;
        if self.user_stack_size == 0 || !self.user_stack_size.is_multiple_of(FRAME_SIZE) {
            return Err(UserImageError::MisalignedStack);
        }
        if !self.user_stack_top.as_usize().is_multiple_of(FRAME_SIZE) {
            return Err(UserImageError::MisalignedStack);
        }
        let stack_base = self.user_stack_base()?;
        let stack_end = self.user_stack_top.as_usize();

        let mut found_entry_in_exec = false;
        for (i, seg) in self.segments.iter().enumerate() {
            if seg.mapped_size == 0 || !seg.mapped_size.is_multiple_of(FRAME_SIZE) {
                return Err(UserImageError::MisalignedSegment);
            }
            if !seg.va_base.as_usize().is_multiple_of(FRAME_SIZE) {
                return Err(UserImageError::MisalignedSegment);
            }
            if seg.init_bytes.len() > seg.mapped_size {
                return Err(UserImageError::InitBytesExceedMapped);
            }

            let seg_start = seg.va_base.as_usize();
            let seg_end = seg_start + seg.mapped_size;

            // Перекрытие со стеком.
            if seg_end > stack_base.as_usize() && stack_end > seg_start {
                return Err(UserImageError::StackOverlapsSegment);
            }

            // Попарное перекрытие сегментов.
            for other in &self.segments[..i] {
                let other_start = other.va_base.as_usize();
                let other_end = other_start + other.mapped_size;
                if seg_end > other_start && other_end > seg_start {
                    return Err(UserImageError::OverlappingSegments);
                }
            }

            // Entry должен лежать в исполняемом сегменте.
            let entry = self.entry.as_usize();
            if entry >= seg_start && entry < seg_end && segment_is_executable(seg.perms) {
                found_entry_in_exec = true;
            }
        }

        if !found_entry_in_exec {
            return Err(UserImageError::EntryNotInExecSegment);
        }
        Ok(())
    }
}

fn segment_is_executable(flags: MemFlags) -> bool {
    match flags {
        MemFlags::Private(p) => matches!(p.user.executable, memory::mem_flags::Executable::Allowed),
        MemFlags::Device(_) => false,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const PAGE: usize = 4096;

    fn aligned(addr: usize) -> PageAlignedVirtualAddress {
        PageAlignedVirtualAddress::from_usize(addr).expect("aligned addr in test")
    }

    fn rx_segment<'a>(va: usize, size: usize, init: &'a [u8]) -> UserSegment<'a> {
        UserSegment {
            va_base: aligned(va),
            mapped_size: size,
            init_bytes: init,
            perms: MemFlags::user_rx(),
        }
    }

    fn rw_segment<'a>(va: usize, size: usize, init: &'a [u8]) -> UserSegment<'a> {
        UserSegment {
            va_base: aligned(va),
            mapped_size: size,
            init_bytes: init,
            perms: MemFlags::user_rw(),
        }
    }

    fn make_image<'a>(segs: &'a [UserSegment<'a>], entry: usize) -> UserImage<'a> {
        UserImage {
            segments: segs,
            entry: VirtualAddress::new(entry),
            user_stack_top: VirtualAddress::new(0x1_0000_0000),
            user_stack_size: 4 * PAGE,
        }
    }

    #[test]
    fn validate_accepts_minimal_image() {
        let segs = [rx_segment(0x4000_0000, PAGE, &[0xAA; 8])];
        let image = make_image(&segs, 0x4000_0000);
        assert_eq!(image.validate(), Ok(()));
    }

    #[test]
    fn validate_rejects_misaligned_segment() {
        let init = [0u8; 10];
        let segs = [UserSegment {
            va_base: aligned(0x4000_0000),
            mapped_size: PAGE + 1, // не кратен 4К
            init_bytes: &init,
            perms: MemFlags::user_rx(),
        }];
        let image = make_image(&segs, 0x4000_0000);
        assert_eq!(image.validate(), Err(UserImageError::MisalignedSegment));
    }

    #[test]
    fn validate_rejects_misaligned_stack() {
        let segs = [rx_segment(0x4000_0000, PAGE, &[])];
        let image = UserImage {
            segments: &segs,
            entry: VirtualAddress::new(0x4000_0000),
            user_stack_top: VirtualAddress::new(0x1_0000_0001),
            user_stack_size: 4 * PAGE,
        };
        assert_eq!(image.validate(), Err(UserImageError::MisalignedStack));
    }

    #[test]
    fn validate_rejects_init_bytes_exceed_mapped() {
        let big = [0u8; PAGE + 10];
        let segs = [rx_segment(0x4000_0000, PAGE, &big)];
        let image = make_image(&segs, 0x4000_0000);
        assert_eq!(image.validate(), Err(UserImageError::InitBytesExceedMapped));
    }

    #[test]
    fn validate_rejects_overlapping_segments() {
        let segs = [
            rx_segment(0x4000_0000, 2 * PAGE, &[]),
            rw_segment(0x4000_0000 + PAGE, PAGE, &[]),
        ];
        let image = make_image(&segs, 0x4000_0000);
        assert_eq!(image.validate(), Err(UserImageError::OverlappingSegments));
    }

    #[test]
    fn validate_rejects_stack_overlapping_segment() {
        let segs = [rx_segment(0x1_0000_0000 - 2 * PAGE, 2 * PAGE, &[])];
        let image = UserImage {
            segments: &segs,
            entry: VirtualAddress::new(0x1_0000_0000 - 2 * PAGE),
            user_stack_top: VirtualAddress::new(0x1_0000_0000),
            user_stack_size: 4 * PAGE,
        };
        assert_eq!(image.validate(), Err(UserImageError::StackOverlapsSegment));
    }

    #[test]
    fn validate_rejects_entry_in_non_exec_segment() {
        let segs = [rw_segment(0x4000_0000, PAGE, &[])];
        let image = make_image(&segs, 0x4000_0000);
        assert_eq!(image.validate(), Err(UserImageError::EntryNotInExecSegment));
    }

    #[test]
    fn validate_rejects_entry_outside_segments() {
        let segs = [rx_segment(0x4000_0000, PAGE, &[])];
        let image = make_image(&segs, 0x5000_0000);
        assert_eq!(image.validate(), Err(UserImageError::EntryNotInExecSegment));
    }

    #[test]
    fn validate_accepts_two_non_overlapping_segments() {
        let segs = [
            rx_segment(0x4000_0000, PAGE, &[]),
            rw_segment(0x4000_0000 + PAGE, PAGE, &[]),
        ];
        let image = make_image(&segs, 0x4000_0000);
        assert_eq!(image.validate(), Ok(()));
    }
}
