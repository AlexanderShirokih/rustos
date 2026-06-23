use alloc::{sync::Arc, vec::Vec};

use collections::MutexCell;
use memory::{
    MemFlags, MemoryRegion,
    memory_mapper::MemoryMappingError,
    virtual_address::{PageAlignedVirtualAddress, VirtualAddress},
};

use super::{
    HandleTable,
    errors::{IpcError, SpawnError},
    handle::HandleId,
    resource::Resource,
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
    /// Вершина user-стека.
    pub user_stack_top: VirtualAddress,
    /// Размер user-стека в байтах (4K-кратен).
    pub user_stack_size: usize,
    /// База диапазона user_vm-аллокатора.
    pub user_vm_base: PageAlignedVirtualAddress,
    /// Размер диапазона user_vm-аллокатора в байтах.
    pub user_vm_size: usize,
}

impl UserImageInstall {
    /// Проверяет структурную геометрию образа.
    pub fn validate_geometry(&self) -> Result<(), LoadImageError> {
        const FRAME_SIZE: usize = 4096;
        use LoadImageError::{InvalidGeometry, UserVmRangeOverflow};

        if self.user_stack_size == 0 || !self.user_stack_size.is_multiple_of(FRAME_SIZE) {
            return Err(InvalidGeometry);
        }
        if !self.user_stack_top.as_usize().is_multiple_of(FRAME_SIZE) {
            return Err(InvalidGeometry);
        }
        let stack_base = self
            .user_stack_top
            .as_usize()
            .checked_sub(self.user_stack_size)
            .ok_or(InvalidGeometry)?;
        let stack_end = self.user_stack_top.as_usize();

        let uvm_start = self.user_vm_base.as_usize();
        let uvm_end = uvm_start
            .checked_add(self.user_vm_size)
            .ok_or(UserVmRangeOverflow)?;

        for (i, seg) in self.segments.iter().enumerate() {
            if seg.mapped_size == 0 || !seg.mapped_size.is_multiple_of(FRAME_SIZE) {
                return Err(InvalidGeometry);
            }
            let seg_start = seg.va_base.as_usize();
            let seg_end = seg_start
                .checked_add(seg.mapped_size)
                .ok_or(InvalidGeometry)?;

            // Перекрытие со стеком и с окном user_vm.
            if seg_end > stack_base && stack_end > seg_start {
                return Err(InvalidGeometry);
            }
            if seg_end > uvm_start && uvm_end > seg_start {
                return Err(InvalidGeometry);
            }

            // Попарное перекрытие сегментов.
            for other in &self.segments[..i] {
                let other_start = other.va_base.as_usize();
                let other_end = other_start
                    .checked_add(other.mapped_size)
                    .ok_or(InvalidGeometry)?;
                if seg_end > other_start && other_end > seg_start {
                    return Err(InvalidGeometry);
                }
            }
        }

        // Окно user_vm не должно пересекать стек.
        if uvm_end > stack_base && stack_end > uvm_start {
            return Err(InvalidGeometry);
        }
        Ok(())
    }
}

/// Параметры старта первого user-потока.
pub struct UserStartSpec {
    pub entry: UserThreadEntry,
    pub loader_handle_table: Arc<MutexCell<HandleTable>>,
    pub handle_ids: Vec<HandleId>,
    pub metering_resource: Option<Arc<Resource>>,
}

/// Ошибки [`KernelRuntime::load_user_image_into`].
#[derive(Debug)]
pub enum LoadImageError {
    ProcessNotFound,
    /// Образ уже загружен или у процесса есть потоки/handle'ы.
    WrongState,
    /// Процесс создан в kernel-AS (без mapper-а user-страниц).
    NoUserAddressSpace,
    /// `user_vm_base + user_vm_size` переполняет `usize`.
    UserVmRangeOverflow,
    /// Геометрия образа нарушает инвариант: невыровненный/нулевой стек,
    /// пересечение сегментов между собой, со стеком или с окном user_vm.
    InvalidGeometry,
    /// Ошибка установки PTE для сегмента или стека.
    MappingFailed(MemoryMappingError),
}

#[cfg(test)]
mod tests {
    use super::*;

    fn segmentless(user_stack_size: usize, user_stack_top: usize) -> UserImageInstall {
        UserImageInstall {
            segments: Vec::new(),
            entry: VirtualAddress::new(0x4000_0000),
            user_stack_top: VirtualAddress::new(user_stack_top),
            user_stack_size,
            user_vm_base: PageAlignedVirtualAddress::from_usize(0x1000_0000).expect("aligned"),
            user_vm_size: 0x100_0000,
        }
    }

    #[test]
    fn accepts_aligned_segmentless_image() {
        assert!(segmentless(0x1000, 0x5000_1000).validate_geometry().is_ok());
    }

    #[test]
    fn rejects_zero_stack_size() {
        assert!(matches!(
            segmentless(0, 0x5000_1000).validate_geometry(),
            Err(LoadImageError::InvalidGeometry)
        ));
    }

    #[test]
    fn rejects_unaligned_stack_size() {
        assert!(matches!(
            segmentless(0x1001, 0x5000_1000).validate_geometry(),
            Err(LoadImageError::InvalidGeometry)
        ));
    }

    #[test]
    fn rejects_unaligned_stack_top() {
        assert!(matches!(
            segmentless(0x1000, 0x5000_1001).validate_geometry(),
            Err(LoadImageError::InvalidGeometry)
        ));
    }

    #[test]
    fn rejects_user_vm_range_overflow() {
        let mut image = segmentless(0x1000, 0x5000_1000);
        image.user_vm_base = PageAlignedVirtualAddress::from_usize(0x1000).expect("aligned");
        image.user_vm_size = usize::MAX;
        assert!(matches!(
            image.validate_geometry(),
            Err(LoadImageError::UserVmRangeOverflow)
        ));
    }

    #[test]
    fn rejects_user_vm_window_overlapping_stack() {
        let mut image = segmentless(0x1000, 0x5000_1000);
        image.user_vm_base = PageAlignedVirtualAddress::from_usize(0x5000_0000).expect("aligned");
        image.user_vm_size = 0x2000;
        assert!(matches!(
            image.validate_geometry(),
            Err(LoadImageError::InvalidGeometry)
        ));
    }
}

/// Ошибки [`KernelRuntime::start_user_process`].
#[derive(Debug)]
pub enum StartProcessError {
    ProcessNotFound,
    /// Процесс ещё не загружен или уже стартовал.
    WrongState,
    /// Один из `handle_ids` не найден, не имеет `Rights::TRANSFER` или
    /// дублируется в массиве.
    HandleValidationFailed(IpcError),
    /// Не удалось создать первый user-поток.
    SpawnFailed(SpawnError),
}
