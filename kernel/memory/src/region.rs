//! Регион памяти, хранимый в CapabilityTarget::Memory.

use alloc::{sync::Arc, vec::Vec};
use core::{
    num::NonZeroUsize,
    sync::atomic::{AtomicBool, Ordering},
};

use spin::Mutex;
use syscall::MemoryProfile;

use crate::{
    MemFlags,
    aligned::Aligned,
    frame::Frame,
    frame_allocator::FrameAllocator,
    memory_mapper::{MemoryMapper, MemoryMappingError, MemoryUnmappingError},
    page_round_up,
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

/// Возврат метеринг-бюджета при `Drop` владельца региона - вызывается не более одного
/// раза на `Arc`.
pub trait BudgetRefund: Send + Sync {
    fn refund(&self);
}

/// Отказ смены профиля региона ([`MemoryRegion::with_profile`]).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ProfileChangeError {
    /// `with_profile` отказывает при переходе в `Cached` и при переходе из `Cached`.
    CachedIsFixed,
}

/// PA-бэкинг региона - откуда берётся физическая память и кто ею владеет.
pub enum MemoryBacking {
    /// Анонимные фреймы, выделенные из `FrameAllocator`.
    Virtual {
        owner: Arc<VirtualBacking>,
        first: usize,
    },

    /// Фиксированный PA-диапазон: PA не принадлежит региону. `profile`
    /// различает регистры устройства, некэшируемое окно и cacheable RAM.
    Physical {
        pa_base: PageAlignedAddress,
        profile: MemoryProfile,
    },
}

/// Общее владение фреймами Virtual-региона и его metering-обязательством.
pub struct VirtualBacking {
    frames: Vec<Frame>,
    fa: &'static (dyn FrameAllocator + Send + Sync),
    zeroed: AtomicBool,
    refund: Mutex<Option<Arc<dyn BudgetRefund>>>,
}

/// Ошибки [`MemoryRegion::create_virtual`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum RegionCreateError {
    /// Аллокация фрейма через `fa` отказала. Уже выделенные фреймы возвращены в `fa`.
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
                owner: Arc::new(VirtualBacking {
                    frames,
                    fa,
                    zeroed: AtomicBool::new(false),
                    refund: Mutex::new(None),
                }),
                first: 0,
            },
            size_bytes: pages.get() * PAGE_SIZE,
            access_mask: access,
            refund: None,
        })
    }

    /// Регион поверх фиксированного PA-диапазона Device-MMIO. `access` не должен
    /// разрешать `X` - `debug_assert` проверяет это только в debug-сборке.
    pub fn create_physical_device(
        pa_base: PageAlignedAddress,
        size: NonZeroUsize,
        access: AccessMask,
    ) -> Self {
        debug_assert!(
            !access.allows(AccessMask::X),
            "device memory cannot be executable"
        );
        Self::create_physical(pa_base, size, access, MemoryProfile::Device)
    }

    /// Регион поверх фиксированного PA-диапазона обычной cacheable RAM.
    pub fn create_physical_normal(
        pa_base: PageAlignedAddress,
        size: NonZeroUsize,
        access: AccessMask,
    ) -> Self {
        Self::create_physical(pa_base, size, access, MemoryProfile::Cached)
    }

    fn create_physical(
        pa_base: PageAlignedAddress,
        size: NonZeroUsize,
        access: AccessMask,
        profile: MemoryProfile,
    ) -> Self {
        Self {
            backing: MemoryBacking::Physical { pa_base, profile },
            size_bytes: size.get(),
            access_mask: access,
            refund: None,
        }
    }

    /// Возвращает регион поверх того же PA-диапазона с профилем `profile`.
    /// Профиль меняется в пределах [`MemoryProfile::WriteCombining`] и
    /// [`MemoryProfile::Device`]. Результат разделяет владение бэкингом с
    /// исходным регионом и отдельного `refund` не хранит.
    pub fn with_profile(&self, profile: MemoryProfile) -> Result<Self, ProfileChangeError> {
        match self.backing {
            MemoryBacking::Physical {
                pa_base,
                profile: current,
            } if current != MemoryProfile::Cached && profile != MemoryProfile::Cached => Ok(Self {
                backing: MemoryBacking::Physical { pa_base, profile },
                size_bytes: self.size_bytes,
                access_mask: self.access_mask,
                refund: None,
            }),
            _ => Err(ProfileChangeError::CachedIsFixed),
        }
    }

    /// Возвращает под-регион `[offset_bytes, offset_bytes + size)` с маской
    /// доступа `access`. `offset_bytes` должен быть выровнен на страницу, окно
    /// должно умещаться внутри исходного региона, `access` - не шире исходного.
    /// Результат разделяет владение бэкингом с исходным регионом и отдельного `refund` не хранит.
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
            MemoryBacking::Physical { pa_base, profile } => {
                let sub_pa = pa_base
                    .as_usize()
                    .checked_add(offset_bytes)
                    .and_then(PageAlignedAddress::from_usize)
                    .ok_or(RegionSliceError::OutOfBounds)?;
                Ok(Self::create_physical(sub_pa, size, access, *profile))
            }
            MemoryBacking::Virtual { owner, first } => Ok(Self {
                backing: MemoryBacking::Virtual {
                    owner: Arc::clone(owner),
                    first: first + offset_bytes / PAGE_SIZE,
                },
                size_bytes: size.get(),
                access_mask: access,
                refund: None,
            }),
        }
    }

    /// Навешивает метеринг-обязательство (см. [`BudgetRefund`]).
    pub fn with_refund(mut self, refund: Arc<dyn BudgetRefund>) -> Self {
        match &mut self.backing {
            MemoryBacking::Virtual { owner, .. } => {
                *owner.refund.lock() = Some(refund);
            }
            MemoryBacking::Physical { .. } => self.refund = Some(refund),
        }
        self
    }

    pub fn size_bytes(&self) -> usize {
        self.size_bytes
    }

    pub fn access_mask(&self) -> AccessMask {
        self.access_mask
    }

    /// Проверяет, что `sub` ссылается на вложенное окно того же бэкинга и не расширяет доступ.
    pub fn contains_subregion(&self, sub: &Self) -> bool {
        if !self.access_mask.allows(sub.access_mask) {
            return false;
        }
        match (&self.backing, &sub.backing) {
            (
                MemoryBacking::Physical {
                    pa_base: origin, ..
                },
                MemoryBacking::Physical {
                    pa_base: nested, ..
                },
            ) => contains_range(
                origin.as_usize(),
                self.size_bytes,
                nested.as_usize(),
                sub.size_bytes,
            ),
            (
                MemoryBacking::Virtual {
                    owner: origin,
                    first: origin_first,
                },
                MemoryBacking::Virtual {
                    owner: nested,
                    first: nested_first,
                },
            ) => {
                Arc::ptr_eq(origin, nested)
                    && contains_range(
                        origin_first * PAGE_SIZE,
                        self.size_bytes,
                        nested_first * PAGE_SIZE,
                        sub.size_bytes,
                    )
            }
            _ => false,
        }
    }

    /// Тег варианта backing, стабильный для ABI: `1` - Virtual, `2` - Physical.
    /// Значение зашито в пользовательский wire-формат - смена чисел ломает ABI.
    pub fn kind_tag(&self) -> u8 {
        match self.backing {
            MemoryBacking::Virtual { .. } => 1,
            MemoryBacking::Physical { .. } => 2,
        }
    }

    /// Базовый PA для `Physical`-региона. `None` для анонимного `Virtual`.
    /// Userspace сопоставляет выданный device-регион с FDT-узлом по этому PA.
    pub fn physical_base(&self) -> Option<PageAlignedAddress> {
        match self.backing {
            MemoryBacking::Physical { pa_base, .. } => Some(pa_base),
            MemoryBacking::Virtual { .. } => None,
        }
    }

    /// Профиль работы с памятью региона. `Virtual` всегда `Cached`. Для
    /// `Physical` хранится в бэкинге.
    pub fn profile(&self) -> MemoryProfile {
        match self.backing {
            MemoryBacking::Virtual { .. } => MemoryProfile::Cached,
            MemoryBacking::Physical { profile, .. } => profile,
        }
    }

    /// Маппит регион в `mapper` начиная с `va` со флагами `flags`. Для `Virtual`-бэкинга
    /// маппит по фрейму в цикле и откатывает уже замапленные leaf-страницы при ошибке
    /// посередине. Для `Physical`-бэкинга это один вызов `map_exact` на весь диапазон -
    /// откат частичного отказа определяет реализация `mapper`.
    pub fn install(
        &self,
        mapper: &dyn MemoryMapper,
        va: PageAlignedVirtualAddress,
        flags: MemFlags,
    ) -> Result<(), MemoryMappingError> {
        match &self.backing {
            MemoryBacking::Virtual { owner, first } => {
                if !owner.zeroed.swap(true, Ordering::AcqRel) {
                    for frame in &owner.frames {
                        mapper.zero_owned_frame(frame.page_address());
                    }
                }
                let pages = self.size_bytes / PAGE_SIZE;
                let frames = owner
                    .frames
                    .get(*first..*first + pages)
                    .ok_or(MemoryMappingError::VirtualMappingError)?;
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
            MemoryBacking::Physical { pa_base, .. } => {
                mapper.map_exact(va, *pa_base, self.size_bytes, flags)
            }
        }
    }

    /// Снимает маппинг всех страниц, которые занял `install`. Не возвращает фреймы.
    pub fn uninstall(
        &self,
        mapper: &dyn MemoryMapper,
        va: PageAlignedVirtualAddress,
    ) -> Result<(), MemoryUnmappingError> {
        let size = NonZeroUsize::new(self.size_bytes)
            .and_then(page_round_up)
            .ok_or(MemoryUnmappingError::MisalignedRange)?;
        mapper.unmap(va, size.get())
    }
}

fn contains_range(origin: usize, origin_size: usize, nested: usize, nested_size: usize) -> bool {
    let (Some(origin_end), Some(nested_end)) = (
        origin.checked_add(origin_size),
        nested.checked_add(nested_size),
    ) else {
        return false;
    };
    origin <= nested && nested_end <= origin_end
}

impl Drop for MemoryRegion {
    fn drop(&mut self) {
        if let Some(refund) = self.refund.take() {
            refund.refund();
        }
    }
}

impl Drop for VirtualBacking {
    fn drop(&mut self) {
        for frame in self.frames.drain(..) {
            let _ = self.fa.deallocate_frame(frame);
        }
        if let Some(refund) = self.refund.lock().take() {
            refund.refund();
        }
    }
}
