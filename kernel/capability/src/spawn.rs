use alloc::{sync::Arc, vec::Vec};

use collections::MutexCell;
use memory::{
    MemFlags, MemoryRegion, PAGE_SIZE,
    memory_mapper::MemoryMappingError,
    virtual_address::{PageAlignedVirtualAddress, VirtualAddress},
};

use super::{
    HandleTable,
    errors::{IpcError, SpawnError},
    handle::HandleId,
    process::MeteringGrant,
    runtime::UserThreadEntry,
};

/// Сегмент образа, готовый к установке в child AS.
pub struct UserSegmentInstall {
    /// Базовый VA сегмента в child AS.
    pub va_base: PageAlignedVirtualAddress,
    /// Размер региона в байтах (равен `region.size_bytes()`).
    pub mapped_size: usize,
    /// Регион, чьи фреймы будут установлены в child AS.
    pub region: Arc<MemoryRegion>,
    /// Флаги маппинга в child AS.
    pub flags: MemFlags,
}

/// Описание образа, которое планировщик установит в child AS.
pub struct UserImageInstall {
    pub segments: Vec<UserSegmentInstall>,
    /// PC первого user-инструкции.
    pub entry: VirtualAddress,
    /// База диапазона user_vm-аллокатора.
    pub user_vm_base: PageAlignedVirtualAddress,
    /// Размер диапазона user_vm-аллокатора в байтах.
    pub user_vm_size: usize,
}

impl UserImageInstall {
    /// Проверяет структурную геометрию образа.
    pub fn validate_geometry(&self) -> Result<(), LoadImageError> {
        use LoadImageError::{InvalidGeometry, UserVmRangeOverflow};

        let uvm_start = self.user_vm_base.as_usize();
        let uvm_end = uvm_start
            .checked_add(self.user_vm_size)
            .ok_or(UserVmRangeOverflow)?;

        for (i, seg) in self.segments.iter().enumerate() {
            if seg.mapped_size == 0 || !seg.mapped_size.is_multiple_of(PAGE_SIZE.get()) {
                return Err(InvalidGeometry);
            }
            let seg_start = seg.va_base.as_usize();
            let seg_end = seg_start
                .checked_add(seg.mapped_size)
                .ok_or(InvalidGeometry)?;

            // Перекрытие с окном user_vm.
            if seg_end > uvm_start && uvm_end > seg_start {
                return Err(InvalidGeometry);
            }

            // Попарное перекрытие сегментов.
            for other in self.segments.iter().take(i) {
                let other_start = other.va_base.as_usize();
                let other_end = other_start
                    .checked_add(other.mapped_size)
                    .ok_or(InvalidGeometry)?;
                if seg_end > other_start && other_end > seg_start {
                    return Err(InvalidGeometry);
                }
            }
        }

        Ok(())
    }
}

/// Параметры старта первого user-потока.
pub struct UserStartSpec {
    pub entry: UserThreadEntry,
    pub loader_handle_table: Arc<MutexCell<HandleTable>>,
    pub handle_id: HandleId,
    pub metering_grant: Option<MeteringGrant>,
}

/// Ошибки `KernelRuntime::load_user_image_into`.
#[derive(Debug)]
pub enum LoadImageError {
    ProcessNotFound,
    /// Образ уже загружен или у процесса есть потоки либо handle.
    WrongState,
    /// Процесс создан в kernel-AS (без mapper-а user-страниц).
    NoUserAddressSpace,
    /// `user_vm_base + user_vm_size` переполняет `usize`.
    UserVmRangeOverflow,
    /// Геометрия образа нарушает инвариант: невыровненный или нулевой размер
    /// сегмента, пересечение сегментов между собой или с окном user_vm.
    InvalidGeometry,
    /// Ошибка установки PTE для сегмента.
    MappingFailed(MemoryMappingError),
}

#[cfg(test)]
mod tests {
    use super::*;

    const SEG_VA: usize = 0x4000_0000;
    const UVM_VA: usize = 0x1000_0000;

    fn segmentless() -> UserImageInstall {
        UserImageInstall {
            segments: Vec::new(),
            entry: VirtualAddress::new(0x4000_0000),
            user_vm_base: PageAlignedVirtualAddress::from_usize(UVM_VA).expect("aligned"),
            user_vm_size: 0x100_0000,
        }
    }

    fn segment(va_base: usize, mapped_size: usize) -> UserSegmentInstall {
        UserSegmentInstall {
            va_base: PageAlignedVirtualAddress::from_usize(va_base).expect("aligned"),
            mapped_size,
            // Геометрию считают поля выше: сам регион в ней не участвует.
            region: Arc::new(MemoryRegion::create_physical_normal(
                memory::physical_address::PageAlignedAddress::from_usize(0x8000_0000)
                    .expect("aligned"),
                core::num::NonZeroUsize::new(PAGE_SIZE.get()).expect("non-zero"),
                memory::AccessMask::RW,
            )),
            flags: MemFlags::user_rw(),
        }
    }

    fn with_segments(segments: Vec<UserSegmentInstall>) -> UserImageInstall {
        UserImageInstall {
            segments,
            ..segmentless()
        }
    }

    #[test]
    fn accepts_aligned_segmentless_image() {
        assert!(segmentless().validate_geometry().is_ok());
    }

    #[test]
    fn accepts_disjoint_segments() {
        let image = with_segments(vec![
            segment(SEG_VA, PAGE_SIZE.get()),
            segment(SEG_VA + PAGE_SIZE.get(), PAGE_SIZE.get()),
        ]);
        assert!(image.validate_geometry().is_ok());
    }

    #[test]
    fn rejects_zero_segment_size() {
        assert!(matches!(
            with_segments(vec![segment(SEG_VA, 0)]).validate_geometry(),
            Err(LoadImageError::InvalidGeometry)
        ));
    }

    #[test]
    fn rejects_unaligned_segment_size() {
        assert!(matches!(
            with_segments(vec![segment(SEG_VA, PAGE_SIZE.get() + 1)]).validate_geometry(),
            Err(LoadImageError::InvalidGeometry)
        ));
    }

    #[test]
    fn rejects_overlapping_segments() {
        let image = with_segments(vec![
            segment(SEG_VA, 2 * PAGE_SIZE.get()),
            segment(SEG_VA + PAGE_SIZE.get(), PAGE_SIZE.get()),
        ]);
        assert!(matches!(
            image.validate_geometry(),
            Err(LoadImageError::InvalidGeometry)
        ));
    }

    #[test]
    fn rejects_segment_overlapping_user_vm_window() {
        assert!(matches!(
            with_segments(vec![segment(UVM_VA, PAGE_SIZE.get())]).validate_geometry(),
            Err(LoadImageError::InvalidGeometry)
        ));
    }

    #[test]
    fn rejects_user_vm_range_overflow() {
        let mut image = segmentless();
        image.user_vm_base = PageAlignedVirtualAddress::from_usize(0x1000).expect("aligned");
        image.user_vm_size = usize::MAX;
        assert!(matches!(
            image.validate_geometry(),
            Err(LoadImageError::UserVmRangeOverflow)
        ));
    }
}

/// Ошибки `KernelRuntime::start_user_process`.
#[derive(Debug)]
pub enum StartProcessError {
    ProcessNotFound,
    /// Процесс ещё не загружен или уже стартовал.
    WrongState,
    /// `handle_id` не найден или не имеет `Rights::TRANSFER`.
    HandleValidationFailed(IpcError),
    /// Не удалось создать первый user-поток.
    SpawnFailed(SpawnError),
}
