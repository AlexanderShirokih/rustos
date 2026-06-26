//! Регион памяти, хранимый в CapabilityTarget::Memory.

use alloc::{sync::Arc, vec::Vec};
use core::{
    num::NonZeroUsize,
    sync::atomic::{AtomicBool, Ordering},
};

use crate::{
    MemFlags,
    aligned::Aligned,
    frame::Frame,
    frame_allocator::FrameAllocator,
    memory_mapper::{MemoryMapper, MemoryMappingError, MemoryUnmappingError},
    physical_address::PageAlignedAddress,
    virtual_address::PageAlignedVirtualAddress,
};

const PAGE_SIZE: usize = PageAlignedVirtualAddress::ALIGNMENT;

/// Маска допустимого обращения к региону: R/W/X.
#[repr(transparent)]
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct AccessMask(u8);

impl AccessMask {
    pub const NONE: Self = Self(0);
    pub const R: Self = Self(1 << 0);
    pub const W: Self = Self(1 << 1);
    pub const X: Self = Self(1 << 2);
    pub const RW: Self = Self(Self::R.0 | Self::W.0);
    pub const RX: Self = Self(Self::R.0 | Self::X.0);
    pub const RWX: Self = Self(Self::R.0 | Self::W.0 | Self::X.0);

    const ALL_BITS: u8 = Self::R.0 | Self::W.0 | Self::X.0;

    pub const fn from_bits_truncate(bits: u8) -> Self {
        Self(bits & Self::ALL_BITS)
    }

    pub const fn bits(self) -> u8 {
        self.0
    }

    pub const fn allows(self, requested: Self) -> bool {
        (self.0 & requested.0) == requested.0
    }
}

/// Возврат метеринг-бюджета на `Drop` региона.
pub trait BudgetRefund: Send + Sync {
    fn refund(&self);
}

/// Тип памяти региона
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum MemoryType {
    Normal,
    Device,
}

/// PA-бэкинг региона.
pub enum MemoryBacking {
    /// Анонимные фреймы, выделенные из `FrameAllocator`.
    Virtual {
        frames: Vec<Frame>,
        fa: &'static (dyn FrameAllocator + Send + Sync),
        zeroed: AtomicBool,
    },

    /// Фиксированный PA-диапазон Device-MMIO: PA не принадлежит региону.
    Physical { pa_base: PageAlignedAddress },
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum RegionCreateError {
    OutOfMemory,
}

/// Отказ нарезки под-региона ([`MemoryRegion::slice`]).
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum RegionSliceError {
    /// Окно `[offset, offset + size)` выходит за пределы исходного региона.
    OutOfBounds,
    /// Смещение нарезки не выровнено на страницу.
    MisalignedOffset,
    /// Запрошенный доступ шире исходного.
    AccessEscalation,
    /// Бэкинг исходного региона не поддерживает нарезку.
    UnsupportedBacking,
}

pub struct MemoryRegion {
    backing: MemoryBacking,
    size_bytes: usize,
    access_mask: AccessMask,
    refund: Option<Arc<dyn BudgetRefund>>,
}

impl MemoryRegion {
    /// Eagerly выделяет `pages` фреймов из `fa` и оборачивает их в Virtual-регион.
    /// На любой ошибке аллокации все уже выделенные фреймы возвращаются в `fa`.
    pub fn create_virtual(
        fa: &'static (dyn FrameAllocator + Send + Sync),
        pages: NonZeroUsize,
        access: AccessMask,
    ) -> Result<Self, RegionCreateError> {
        let mut frames: Vec<Frame> = Vec::with_capacity(pages.get());
        for _ in 0..pages.get() {
            if let Some(frame) = fa.allocate_frame() {
                frames.push(frame);
            } else {
                for f in frames.drain(..) {
                    let _ = fa.deallocate_frame(f);
                }
                return Err(RegionCreateError::OutOfMemory);
            }
        }
        Ok(Self {
            backing: MemoryBacking::Virtual {
                frames,
                fa,
                zeroed: AtomicBool::new(false),
            },
            size_bytes: pages.get() * PAGE_SIZE,
            access_mask: access,
            refund: None,
        })
    }

    /// Регион поверх фиксированного PA-диапазона Device-MMIO.
    pub fn create_physical_device(
        pa_base: PageAlignedAddress,
        size: NonZeroUsize,
        access: AccessMask,
    ) -> Self {
        debug_assert!(
            !access.allows(AccessMask::X),
            "device memory cannot be executable"
        );
        Self {
            backing: MemoryBacking::Physical { pa_base },
            size_bytes: size.get(),
            access_mask: access,
            refund: None,
        }
    }

    /// Возвращает под-регион `[offset_bytes, offset_bytes + size)` с маской
    /// доступа `access`. `offset_bytes` должен быть выровнен на страницу, окно
    /// должно умещаться внутри исходного региона, `access` - не шире исходного.
    /// Результат не несёт `refund`. Бэкинг `Virtual` не поддерживается.
    pub fn slice(
        &self,
        offset_bytes: usize,
        size: NonZeroUsize,
        access: AccessMask,
    ) -> Result<Self, RegionSliceError> {
        if !self.access_mask.allows(access) {
            return Err(RegionSliceError::AccessEscalation);
        }
        if !offset_bytes.is_multiple_of(PAGE_SIZE) {
            return Err(RegionSliceError::MisalignedOffset);
        }
        let end = offset_bytes
            .checked_add(size.get())
            .ok_or(RegionSliceError::OutOfBounds)?;
        if end > self.size_bytes {
            return Err(RegionSliceError::OutOfBounds);
        }

        match &self.backing {
            MemoryBacking::Physical { pa_base } => {
                let sub_pa = pa_base
                    .as_usize()
                    .checked_add(offset_bytes)
                    .and_then(PageAlignedAddress::from_usize)
                    .ok_or(RegionSliceError::OutOfBounds)?;
                Ok(Self {
                    backing: MemoryBacking::Physical { pa_base: sub_pa },
                    size_bytes: size.get(),
                    access_mask: access,
                    refund: None,
                })
            }
            MemoryBacking::Virtual { .. } => Err(RegionSliceError::UnsupportedBacking),
        }
    }

    /// Навешивает метеринг-обязательство (см. [`BudgetRefund`]).
    pub fn with_refund(mut self, refund: Arc<dyn BudgetRefund>) -> Self {
        self.refund = Some(refund);
        self
    }

    pub fn size_bytes(&self) -> usize {
        self.size_bytes
    }

    pub fn access_mask(&self) -> AccessMask {
        self.access_mask
    }

    /// Численный 8-битный тег вариантов backing.
    pub fn kind_tag(&self) -> u8 {
        match self.backing {
            MemoryBacking::Virtual { .. } => 1,
            MemoryBacking::Physical { .. } => 2,
        }
    }

    /// Базовый PA для `Physical`-региона; `None` для анонимного `Virtual`.
    /// Userspace сопоставляет выданный device-регион с FDT-узлом по этому PA.
    pub fn physical_base(&self) -> Option<PageAlignedAddress> {
        match self.backing {
            MemoryBacking::Physical { pa_base } => Some(pa_base),
            MemoryBacking::Virtual { .. } => None,
        }
    }

    /// Тип памяти, выведенный из бэкинга.
    pub fn memory_type(&self) -> MemoryType {
        match self.backing {
            MemoryBacking::Virtual { .. } => MemoryType::Normal,
            MemoryBacking::Physical { .. } => MemoryType::Device,
        }
    }

    /// Маппит регион в `mapper` начиная с `va` со флагами `flags`.
    /// При ошибке посередине цикла уже замапленные leaf-страницы откатываются.
    pub fn install(
        &self,
        mapper: &dyn MemoryMapper,
        va: PageAlignedVirtualAddress,
        flags: MemFlags,
    ) -> Result<(), MemoryMappingError> {
        match &self.backing {
            MemoryBacking::Virtual { frames, zeroed, .. } => {
                if !zeroed.swap(true, Ordering::AcqRel) {
                    for frame in frames {
                        mapper.zero_owned_frame(frame.page_address());
                    }
                }
                for (i, frame) in frames.iter().enumerate() {
                    let target_va = va
                        .offset(i * PAGE_SIZE)
                        .ok_or(MemoryMappingError::VirtualMappingError)?;
                    if let Err(e) =
                        mapper.map_exact(target_va, frame.page_address(), PAGE_SIZE, flags)
                    {
                        for j in 0..i {
                            if let Some(prev_va) = va.offset(j * PAGE_SIZE) {
                                let _ = mapper.unmap(prev_va, PAGE_SIZE);
                            }
                        }
                        return Err(e);
                    }
                }
                Ok(())
            }
            MemoryBacking::Physical { pa_base } => {
                mapper.map_exact(va, *pa_base, self.size_bytes, flags)
            }
        }
    }

    /// Снимает маппинг. Не возвращает фреймы.
    pub fn uninstall(
        &self,
        mapper: &dyn MemoryMapper,
        va: PageAlignedVirtualAddress,
    ) -> Result<(), MemoryUnmappingError> {
        mapper.unmap(va, self.size_bytes)
    }
}

impl Drop for MemoryRegion {
    fn drop(&mut self) {
        // Virtual владеет фреймами - возвращает их в FA.
        if let MemoryBacking::Virtual { frames, fa, .. } = &mut self.backing {
            for frame in frames.drain(..) {
                let _ = fa.deallocate_frame(frame);
            }
        }

        if let Some(refund) = self.refund.take() {
            refund.refund();
        }
    }
}

#[cfg(test)]
mod tests {
    extern crate std;

    use std::{sync::Mutex, vec::Vec as StdVec};

    use super::*;
    use crate::{
        frame_allocator::{FrameError, ReserveFrameError},
        memory_mapper::{AddressSpaceHandle, AddressSpaceTag},
        physical_address::PhysicalAddress,
    };

    struct MockFrameAllocator {
        next: Mutex<usize>,
        deallocated: Mutex<StdVec<Frame>>,
        allocate_limit: Mutex<usize>,
    }

    impl MockFrameAllocator {
        fn new() -> Self {
            Self {
                next: Mutex::new(100),
                deallocated: Mutex::new(StdVec::new()),
                allocate_limit: Mutex::new(usize::MAX),
            }
        }

        fn with_limit(limit: usize) -> Self {
            let me = Self::new();
            *me.allocate_limit.lock().unwrap() = limit;
            me
        }

        fn deallocated(&self) -> StdVec<Frame> {
            self.deallocated.lock().unwrap().clone()
        }
    }

    impl FrameAllocator for MockFrameAllocator {
        fn reserve_frames_exact(
            &self,
            from_inclusive: Frame,
            _to_exclusive: Frame,
        ) -> Result<Frame, ReserveFrameError> {
            Ok(from_inclusive)
        }

        fn allocate_frame(&self) -> Option<Frame> {
            let mut limit = self.allocate_limit.lock().unwrap();
            if *limit == 0 {
                return None;
            }
            *limit -= 1;
            drop(limit);
            let mut n = self.next.lock().unwrap();
            let value = *n;
            *n += 1;
            Some(Frame::new(value))
        }

        fn allocate_frames(&self, _max_count: usize) -> Option<(Frame, usize)> {
            None
        }

        fn deallocate_frame(&self, frame: Frame) -> Result<(), FrameError> {
            self.deallocated.lock().unwrap().push(frame);
            Ok(())
        }

        fn is_allocated(&self, _frame: Frame) -> bool {
            false
        }
    }

    #[derive(Default)]
    struct MockMapper {
        mapped: Mutex<StdVec<(usize, usize, usize)>>,
        unmapped: Mutex<StdVec<(usize, usize)>>,
        zeroed: Mutex<StdVec<usize>>,
        fail_after: Mutex<Option<usize>>,
    }

    impl MockMapper {
        fn install_failing_after(&self, n: usize) {
            *self.fail_after.lock().unwrap() = Some(n);
        }
    }

    impl MemoryMapper for MockMapper {
        fn map(
            &self,
            _va: PageAlignedVirtualAddress,
            _page_count: usize,
            _init: &[u8],
            _flags: MemFlags,
        ) -> Result<(), MemoryMappingError> {
            Err(MemoryMappingError::VirtualMappingError)
        }

        fn map_exact(
            &self,
            source_address: PageAlignedVirtualAddress,
            target_address: PageAlignedAddress,
            size: usize,
            _mem_flags: MemFlags,
        ) -> Result<(), MemoryMappingError> {
            if let Some(limit) = *self.fail_after.lock().unwrap()
                && self.mapped.lock().unwrap().len() >= limit
            {
                return Err(MemoryMappingError::OutOfMemory);
            }
            self.mapped.lock().unwrap().push((
                source_address.as_usize(),
                target_address.as_usize(),
                size,
            ));
            Ok(())
        }

        fn unmap(
            &self,
            address: PageAlignedVirtualAddress,
            size: usize,
        ) -> Result<(), MemoryUnmappingError> {
            self.unmapped
                .lock()
                .unwrap()
                .push((address.as_usize(), size));
            Ok(())
        }

        fn remap(
            &self,
            _start_address: PageAlignedVirtualAddress,
            _size: usize,
            _new_flags: MemFlags,
        ) -> Result<(), crate::memory_mapper::MemoryRemappingError> {
            Ok(())
        }

        fn activate_handle(&self) -> AddressSpaceHandle {
            AddressSpaceHandle::new(PhysicalAddress::new(0), AddressSpaceTag::NONE)
        }

        fn zero_owned_frame(&self, pa: PageAlignedAddress) {
            self.zeroed.lock().unwrap().push(pa.as_usize());
        }

        fn as_any(&self) -> &(dyn core::any::Any + 'static) {
            self
        }
    }

    fn nz(v: usize) -> NonZeroUsize {
        NonZeroUsize::new(v).unwrap()
    }

    fn aligned_va(addr: usize) -> PageAlignedVirtualAddress {
        PageAlignedVirtualAddress::from_usize(addr).unwrap()
    }

    fn aligned_pa(addr: usize) -> PageAlignedAddress {
        PageAlignedAddress::from_usize(addr).unwrap()
    }

    fn leak_fa() -> &'static MockFrameAllocator {
        std::boxed::Box::leak(std::boxed::Box::new(MockFrameAllocator::new()))
    }

    #[test]
    fn access_mask_allows_all_combinations() {
        assert!(AccessMask::RW.allows(AccessMask::R));
        assert!(AccessMask::RW.allows(AccessMask::W));
        assert!(AccessMask::RW.allows(AccessMask::RW));
        assert!(!AccessMask::RW.allows(AccessMask::X));
        assert!(!AccessMask::RW.allows(AccessMask::RX));
        assert!(AccessMask::RWX.allows(AccessMask::RWX));
        assert!(AccessMask::RX.allows(AccessMask::R));
        assert!(AccessMask::RX.allows(AccessMask::X));
        assert!(!AccessMask::RX.allows(AccessMask::W));
        assert!(AccessMask::NONE.allows(AccessMask::NONE));
        assert!(!AccessMask::NONE.allows(AccessMask::R));
    }

    #[test]
    fn access_mask_from_bits_truncates_unknown() {
        let masked = AccessMask::from_bits_truncate(0xFF);
        assert_eq!(masked.bits(), AccessMask::RWX.bits());
    }

    #[test]
    fn virtual_drop_returns_frames_to_allocator() {
        let fa = leak_fa();
        let region =
            MemoryRegion::create_virtual(fa, nz(3), AccessMask::RW).expect("alloc must succeed");
        assert_eq!(region.size_bytes(), 3 * PAGE_SIZE);
        assert_eq!(region.kind_tag(), 1);
        let allocated_frames: StdVec<Frame> = match &region.backing {
            MemoryBacking::Virtual { frames, .. } => frames.clone(),
            MemoryBacking::Physical { .. } => panic!("expected Virtual"),
        };
        assert_eq!(fa.deallocated().len(), 0);
        drop(region);
        assert_eq!(fa.deallocated(), allocated_frames);
    }

    #[test]
    fn virtual_create_oom_returns_all_frames() {
        let fa = std::boxed::Box::leak(std::boxed::Box::new(MockFrameAllocator::with_limit(2)));
        let err = MemoryRegion::create_virtual(fa, nz(5), AccessMask::RW);
        assert!(err.is_err());
        assert_eq!(err.err().unwrap(), RegionCreateError::OutOfMemory);
        assert_eq!(fa.deallocated().len(), 2);
    }

    #[test]
    fn slice_physical_narrows_pa_size_and_access() {
        let region = MemoryRegion::create_physical_device(
            aligned_pa(0x4000_0000),
            nz(0x4000),
            AccessMask::RW,
        );
        assert_eq!(region.memory_type(), MemoryType::Device);
        let sub = region
            .slice(0x1000, nz(0x1000), AccessMask::R)
            .expect("in-bounds slice with subset access");
        assert_eq!(sub.kind_tag(), 2);
        assert_eq!(sub.size_bytes(), 0x1000);
        assert_eq!(sub.access_mask().bits(), AccessMask::R.bits());
        assert_eq!(sub.memory_type(), MemoryType::Device);
        match &sub.backing {
            MemoryBacking::Physical { pa_base } => assert_eq!(pa_base.as_usize(), 0x4000_1000),
            MemoryBacking::Virtual { .. } => panic!("physical slice must stay physical"),
        }
    }

    #[test]
    fn virtual_region_is_normal_type() {
        let fa = std::boxed::Box::leak(std::boxed::Box::new(MockFrameAllocator::with_limit(1)));
        let region = MemoryRegion::create_virtual(fa, nz(1), AccessMask::RW).expect("alloc");
        assert_eq!(region.memory_type(), MemoryType::Normal);
    }

    #[test]
    fn slice_rejects_window_past_region_end() {
        let region = MemoryRegion::create_physical_device(
            aligned_pa(0x4000_0000),
            nz(0x4000),
            AccessMask::RW,
        );
        assert!(matches!(
            region.slice(0x3000, nz(0x2000), AccessMask::R),
            Err(RegionSliceError::OutOfBounds)
        ));
    }

    #[test]
    fn slice_rejects_access_wider_than_region() {
        let region = MemoryRegion::create_physical_device(
            aligned_pa(0x4000_0000),
            nz(0x4000),
            AccessMask::R,
        );
        assert!(matches!(
            region.slice(0, nz(0x1000), AccessMask::RW),
            Err(RegionSliceError::AccessEscalation)
        ));
    }

    #[test]
    fn slice_rejects_misaligned_offset() {
        let region = MemoryRegion::create_physical_device(
            aligned_pa(0x4000_0000),
            nz(0x4000),
            AccessMask::RW,
        );
        assert!(matches!(
            region.slice(0x800, nz(0x1000), AccessMask::R),
            Err(RegionSliceError::MisalignedOffset)
        ));
    }

    #[test]
    fn slice_of_virtual_is_unsupported() {
        let fa = leak_fa();
        let region = MemoryRegion::create_virtual(fa, nz(2), AccessMask::RW).unwrap();
        assert!(matches!(
            region.slice(0, nz(PAGE_SIZE), AccessMask::R),
            Err(RegionSliceError::UnsupportedBacking)
        ));
    }

    #[test]
    fn slice_installs_at_offset_pa() {
        let region = MemoryRegion::create_physical_device(
            aligned_pa(0x8000_0000),
            nz(0x2000),
            AccessMask::RW,
        );
        let sub = region.slice(0x1000, nz(0x1000), AccessMask::R).unwrap();
        let mapper = MockMapper::default();
        let va = aligned_va(0x4000_0000);
        sub.install(&mapper, va, MemFlags::user_ro())
            .expect("install must succeed");
        let entry = mapper.mapped.lock().unwrap()[0];
        assert_eq!(entry.1, 0x8000_1000);
        assert_eq!(entry.2, 0x1000);
    }

    #[test]
    fn physical_base_some_for_physical_none_for_virtual() {
        let phys = MemoryRegion::create_physical_device(
            aligned_pa(0x9010_0000),
            nz(0x1000),
            AccessMask::RW,
        );
        assert_eq!(
            phys.physical_base().map(PageAlignedAddress::as_usize),
            Some(0x9010_0000)
        );

        let fa = leak_fa();
        let virt = MemoryRegion::create_virtual(fa, nz(1), AccessMask::RW).unwrap();
        assert_eq!(virt.physical_base(), None);
    }

    #[test]
    fn physical_drop_does_nothing() {
        let region = MemoryRegion::create_physical_device(
            aligned_pa(0x4000_0000),
            nz(PAGE_SIZE),
            AccessMask::R,
        );
        assert_eq!(region.kind_tag(), 2);
        assert_eq!(region.size_bytes(), PAGE_SIZE);
        drop(region);
    }

    struct CountingRefund(std::sync::atomic::AtomicUsize);

    impl BudgetRefund for CountingRefund {
        fn refund(&self) {
            self.0.fetch_add(1, Ordering::AcqRel);
        }
    }

    #[test]
    fn metered_physical_drop_refunds_exactly_once() {
        let refund = Arc::new(CountingRefund(std::sync::atomic::AtomicUsize::new(0)));
        let region = MemoryRegion::create_physical_device(
            aligned_pa(0x4000_0000),
            nz(PAGE_SIZE),
            AccessMask::R,
        )
        .with_refund(refund.clone());
        // Пока регион жив - refund ещё не сработал.
        assert_eq!(refund.0.load(Ordering::Acquire), 0);
        drop(region);
        // Закрытие региона возвращает бюджет ровно один раз.
        assert_eq!(refund.0.load(Ordering::Acquire), 1);
    }

    #[test]
    fn metered_virtual_drop_refunds_and_returns_frames() {
        let fa = leak_fa();
        let refund = Arc::new(CountingRefund(std::sync::atomic::AtomicUsize::new(0)));
        let region = MemoryRegion::create_virtual(fa, nz(2), AccessMask::RW)
            .expect("alloc must succeed")
            .with_refund(refund.clone());
        assert_eq!(refund.0.load(Ordering::Acquire), 0);
        assert_eq!(fa.deallocated().len(), 0);

        drop(region);
        // Virtual-регион под бюджет: на Drop И возвращает фреймы в FA, И
        // возвращает метеринг-бюджет ровно один раз.
        assert_eq!(refund.0.load(Ordering::Acquire), 1);
        assert_eq!(fa.deallocated().len(), 2);
    }

    #[test]
    fn install_uninstall_round_trip_virtual() {
        let fa = leak_fa();
        let region =
            MemoryRegion::create_virtual(fa, nz(2), AccessMask::RW).expect("alloc must succeed");
        let mapper = MockMapper::default();
        let va = aligned_va(0x4000_0000);
        region
            .install(&mapper, va, MemFlags::user_rw())
            .expect("install must succeed");
        assert_eq!(mapper.mapped.lock().unwrap().len(), 2);
        region
            .uninstall(&mapper, va)
            .expect("uninstall must succeed");
        let unmapped = mapper.unmapped.lock().unwrap().clone();
        assert_eq!(unmapped.len(), 1);
        assert_eq!(unmapped[0].0, va.as_usize());
        assert_eq!(unmapped[0].1, 2 * PAGE_SIZE);
    }

    #[test]
    fn install_uninstall_round_trip_physical() {
        let region = MemoryRegion::create_physical_device(
            aligned_pa(0x8000_0000),
            nz(PAGE_SIZE),
            AccessMask::R,
        );
        let mapper = MockMapper::default();
        let va = aligned_va(0x4000_0000);
        region
            .install(&mapper, va, MemFlags::user_ro())
            .expect("install must succeed");
        assert_eq!(mapper.mapped.lock().unwrap().len(), 1);
        let entry = mapper.mapped.lock().unwrap()[0];
        assert_eq!(entry.0, va.as_usize());
        assert_eq!(entry.1, 0x8000_0000);
        assert_eq!(entry.2, PAGE_SIZE);
        region
            .uninstall(&mapper, va)
            .expect("uninstall must succeed");
    }

    #[test]
    fn install_rolls_back_on_failure() {
        let fa = leak_fa();
        let region =
            MemoryRegion::create_virtual(fa, nz(4), AccessMask::RW).expect("alloc must succeed");
        let mapper = MockMapper::default();
        mapper.install_failing_after(2);
        let va = aligned_va(0x5000_0000);
        let err = region.install(&mapper, va, MemFlags::user_rw());
        assert!(err.is_err());
        assert_eq!(err.err().unwrap(), MemoryMappingError::OutOfMemory);
        let unmapped = mapper.unmapped.lock().unwrap().clone();
        assert_eq!(unmapped.len(), 2);
        assert_eq!(unmapped[0].0, va.as_usize());
        assert_eq!(unmapped[1].0, va.as_usize() + PAGE_SIZE);
    }

    #[test]
    fn access_mask_preserved() {
        let fa = leak_fa();
        let region = MemoryRegion::create_virtual(fa, nz(1), AccessMask::RX).unwrap();
        assert_eq!(region.access_mask().bits(), AccessMask::RX.bits());
    }

    #[test]
    fn install_zeroes_each_frame_on_first_install() {
        let fa = leak_fa();
        let region = MemoryRegion::create_virtual(fa, nz(3), AccessMask::RW).unwrap();
        let mapper = MockMapper::default();
        let va = aligned_va(0x4000_0000);
        region
            .install(&mapper, va, MemFlags::user_rw())
            .expect("install must succeed");
        let zeroed = mapper.zeroed.lock().unwrap().clone();
        assert_eq!(zeroed.len(), 3);
    }

    #[test]
    fn install_after_uninstall_does_not_rezero() {
        let fa = leak_fa();
        let region = MemoryRegion::create_virtual(fa, nz(2), AccessMask::RW).unwrap();
        let mapper = MockMapper::default();
        let va = aligned_va(0x4000_0000);
        region.install(&mapper, va, MemFlags::user_rw()).unwrap();
        region.uninstall(&mapper, va).unwrap();
        region.install(&mapper, va, MemFlags::user_rw()).unwrap();
        let zeroed = mapper.zeroed.lock().unwrap().clone();
        assert_eq!(zeroed.len(), 2);
    }

    #[test]
    fn install_zeroes_before_mapping_on_partial_oom() {
        let fa = leak_fa();
        let region = MemoryRegion::create_virtual(fa, nz(4), AccessMask::RW).unwrap();
        let mapper = MockMapper::default();
        mapper.install_failing_after(2);
        let va = aligned_va(0x5000_0000);
        let _ = region.install(&mapper, va, MemFlags::user_rw());
        // Зануление должно покрыть все фреймы независимо от того, на каком
        // map_exact упадём - иначе следующий install пропустит зануление
        // непоказанных user'у фреймов.
        let zeroed = mapper.zeroed.lock().unwrap().clone();
        assert_eq!(zeroed.len(), 4);

        let mapper2 = MockMapper::default();
        region.install(&mapper2, va, MemFlags::user_rw()).unwrap();
        assert_eq!(mapper2.zeroed.lock().unwrap().len(), 0);
    }

    #[test]
    fn install_physical_does_not_zero() {
        let region = MemoryRegion::create_physical_device(
            aligned_pa(0x8000_0000),
            nz(PAGE_SIZE),
            AccessMask::R,
        );
        let mapper = MockMapper::default();
        region
            .install(&mapper, aligned_va(0x4000_0000), MemFlags::user_ro())
            .unwrap();
        assert_eq!(mapper.zeroed.lock().unwrap().len(), 0);
    }
}
