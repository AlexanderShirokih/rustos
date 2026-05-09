//! Архитектурно-независимый аллокатор виртуального адресного пространства
//! пользовательского процесса.
//!
//! Управляет диапазоном user-VA, отвечающим за per-process "динамическую"
//! область - ту часть нижней половины адресного пространства, которая
//! не занята загруженным образом и пользовательским стеком. Хранит реестр
//! выделенных регионов для операций `set_flags`/`release_pending`.
//!
//! Не выполняет фактический маппинг - это ответственность [`MemoryMapper`].
//! Аллокатор предоставляет только VA: вызывающий передаёт его в
//! `mapper.map(...)`.
//!
//! # Стратегия
//!
//! - **Bump-pointer** - выделяет VA от `region_start` вверх, не пытаясь
//!   повторно использовать освобождённые слоты. Адекватно текущему набору
//!   операций (`vm_allocate` + `vm_remap`); полноценный `vm_free` потребует
//!   стратегии с реюзом - её можно завести без изменения публичного API,
//!   когда появится syscall.

use alloc::vec::Vec;
use core::{
    fmt::{Display, Formatter},
    num::NonZeroUsize,
};

use crate::{
    MemFlags,
    aligned::Aligned,
    virtual_address::{PageAlignedVirtualAddress, VirtualAddress},
};

/// Размер страницы - гранулярность всех операций аллокатора.
const PAGE_SIZE: usize = PageAlignedVirtualAddress::ALIGNMENT;

/// Описатель выделенного региона в user-AS.
///
/// Хранит лишь VA-границы и текущие флаги - фреймы принадлежат
/// `MemoryMapper`. Регионы - закрытое представление для аллокатора.
#[derive(Clone, Copy)]
pub struct UserVmRegion {
    base: PageAlignedVirtualAddress,
    pages: usize,
    flags: MemFlags,
}

impl core::fmt::Debug for UserVmRegion {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        f.debug_struct("UserVmRegion")
            .field("base", &self.base.as_usize())
            .field("pages", &self.pages)
            .finish_non_exhaustive()
    }
}

impl PartialEq for UserVmRegion {
    /// Equality по VA-диапазону: `flags` сравнивать не имеет смысла -
    /// они меняются `set_flags`-ом, оставляя ту же область.
    fn eq(&self, other: &Self) -> bool {
        self.base == other.base && self.pages == other.pages
    }
}

impl Eq for UserVmRegion {}

impl UserVmRegion {
    pub const fn base(&self) -> PageAlignedVirtualAddress {
        self.base
    }

    pub const fn pages(&self) -> usize {
        self.pages
    }

    pub const fn size_bytes(&self) -> usize {
        self.pages * PAGE_SIZE
    }

    pub const fn flags(&self) -> MemFlags {
        self.flags
    }

    /// Конец региона (exclusive).
    pub const fn end_va(&self) -> VirtualAddress {
        VirtualAddress::new(self.base.as_usize() + self.pages * PAGE_SIZE)
    }

    /// `true`, если регион полностью совпадает с диапазоном `[base, base + pages)`.
    fn matches(&self, base: PageAlignedVirtualAddress, pages: usize) -> bool {
        self.base == base && self.pages == pages
    }
}

/// Ошибки выделения user-VA. Нулевой размер исключён на уровне типа -
/// API принимает [`NonZeroUsize`].
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum UserVmAllocateError {
    /// Запрошенный размер слишком велик и не помещается в свободный
    /// хвост user-региона.
    NotEnoughSpace,
    /// Размер запроса не кратен размеру страницы. Аллокатор оперирует
    /// постранично; вызывающий обязан выровнять заранее.
    UnalignedSize,
    /// Реестр регионов исчерпан (исчезающе редкий случай - настраивается
    /// через [`UserVmAllocator::with_capacity`]).
    OutOfSlots,
}

impl Display for UserVmAllocateError {
    fn fmt(&self, f: &mut Formatter<'_>) -> core::fmt::Result {
        match self {
            UserVmAllocateError::NotEnoughSpace => f.write_str("User VA region exhausted"),
            UserVmAllocateError::UnalignedSize => {
                f.write_str("Allocation size must be page-aligned")
            }
            UserVmAllocateError::OutOfSlots => f.write_str("User VM region table is full"),
        }
    }
}

/// Ошибки операций над существующим регионом. Нулевой размер исключён
/// на уровне типа.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum UserVmRegionError {
    /// `(base, pages)` не соответствует ни одному выделенному региону.
    NotFound,
    /// Размер запроса не кратен размеру страницы.
    UnalignedSize,
}

impl Display for UserVmRegionError {
    fn fmt(&self, f: &mut Formatter<'_>) -> core::fmt::Result {
        match self {
            UserVmRegionError::NotFound => f.write_str("No allocated region matches the range"),
            UserVmRegionError::UnalignedSize => f.write_str("Range size must be page-aligned"),
        }
    }
}

/// Per-process аллокатор user-VA.
///
/// Не блокируется внутри - обёртку MutexCell обеспечивает вызывающий
/// (`Process` хранит `Arc<MutexCell<UserVmAllocator>>`).
pub struct UserVmAllocator {
    region_start: PageAlignedVirtualAddress,
    region_end: VirtualAddress,
    next_va: usize,
    regions: Vec<UserVmRegion>,
    max_regions: usize,
}

/// Дефолтная ёмкость реестра регионов.
const DEFAULT_MAX_REGIONS: usize = 64;

impl UserVmAllocator {
    /// Создаёт аллокатор, обслуживающий полуоткрытый диапазон
    /// `[region_start, region_end)`.
    ///
    /// `region_end` должен быть строго больше `region_start.as_usize()` -
    /// иначе аллокатор немедленно вернёт [`UserVmAllocateError::NotEnoughSpace`]
    /// на любой запрос.
    pub fn new(region_start: PageAlignedVirtualAddress, region_end: VirtualAddress) -> Self {
        Self::with_capacity(region_start, region_end, DEFAULT_MAX_REGIONS)
    }

    pub fn with_capacity(
        region_start: PageAlignedVirtualAddress,
        region_end: VirtualAddress,
        max_regions: usize,
    ) -> Self {
        Self {
            region_start,
            region_end,
            next_va: region_start.as_usize(),
            regions: Vec::new(),
            max_regions,
        }
    }

    pub fn region_start(&self) -> PageAlignedVirtualAddress {
        self.region_start
    }

    pub fn region_end(&self) -> VirtualAddress {
        self.region_end
    }

    /// Сколько регионов уже выделено.
    pub fn live_count(&self) -> usize {
        self.regions.len()
    }

    /// Снимок реестра регионов. Используется тестами и diagnostics-путём.
    pub fn regions(&self) -> &[UserVmRegion] {
        &self.regions
    }

    /// Выделяет VA-диапазон длины `size_bytes` (кратной [`PAGE_SIZE`]).
    /// Возвращает базовый VA - вызывающий обязан выполнить маппинг через
    /// [`MemoryMapper::map`].
    ///
    /// Аллокатор не освобождает слот в случае ошибки маппинга - вызывающий
    /// должен явно [`Self::release_pending`], чтобы регион не торчал в
    /// реестре и VA вернулся под последующие выделения.
    pub fn allocate(
        &mut self,
        size_bytes: NonZeroUsize,
        flags: MemFlags,
    ) -> Result<UserVmRegion, UserVmAllocateError> {
        let size_bytes = size_bytes.get();
        if !size_bytes.is_multiple_of(PAGE_SIZE) {
            return Err(UserVmAllocateError::UnalignedSize);
        }
        if self.regions.len() >= self.max_regions {
            return Err(UserVmAllocateError::OutOfSlots);
        }

        let new_end = self
            .next_va
            .checked_add(size_bytes)
            .ok_or(UserVmAllocateError::NotEnoughSpace)?;
        if new_end > self.region_end.as_usize() {
            return Err(UserVmAllocateError::NotEnoughSpace);
        }

        let base = PageAlignedVirtualAddress::from_usize(self.next_va)
            .expect("bump pointer kept page-aligned by construction");
        let region = UserVmRegion {
            base,
            pages: size_bytes / PAGE_SIZE,
            flags,
        };

        self.regions.push(region);
        self.next_va = new_end;
        Ok(region)
    }

    /// Снимает с учёта только что выделенный регион - на пути отката, если
    /// последующий маппинг провалился. `true`, если такой регион был.
    ///
    /// Если откатываемый регион стоит на самой вершине bump'а
    /// (т.е. это **последнее** выделение), то `next_va` тоже откатывается
    /// и VA освобождается под повторный `allocate`. Это безопасно благодаря
    /// атомарности [`MemoryMapper::map`]: при `Err` ни одной leaf-страницы в
    /// page-tables не остаётся. Если регион не последний (между ним и
    /// текущей вершиной были другие `allocate`), bump не двигается - slot
    /// остаётся "дырой" до появления полноценного `vm_free`.
    pub fn release_pending(
        &mut self,
        base: PageAlignedVirtualAddress,
        pages: NonZeroUsize,
    ) -> bool {
        let pages = pages.get();
        let Some(idx) = self.regions.iter().position(|r| r.matches(base, pages)) else {
            return false;
        };
        let region = self.regions.swap_remove(idx);
        if region.end_va().as_usize() == self.next_va {
            self.next_va = region.base().as_usize();
        }
        true
    }

    /// Проверяет, что в реестре зарегистрирован регион `(base, size_bytes)`.
    /// Не модифицирует состояние - для пред-валидации remap-цепочки до
    /// того, как затрагиваются таблицы страниц.
    pub fn lookup(
        &self,
        base: PageAlignedVirtualAddress,
        size_bytes: NonZeroUsize,
    ) -> Result<(), UserVmRegionError> {
        let size_bytes = size_bytes.get();
        if !size_bytes.is_multiple_of(PAGE_SIZE) {
            return Err(UserVmRegionError::UnalignedSize);
        }
        let pages = size_bytes / PAGE_SIZE;
        if self.regions.iter().any(|r| r.matches(base, pages)) {
            Ok(())
        } else {
            Err(UserVmRegionError::NotFound)
        }
    }

    /// Меняет флаги уже зарегистрированного региона. Сам по себе аллокатор
    /// не модифицирует таблицы страниц; вызывающий обязан синхронно
    /// вызвать [`MemoryMapper::remap`] с теми же `(base, size, new_flags)`.
    pub fn set_flags(
        &mut self,
        base: PageAlignedVirtualAddress,
        size_bytes: NonZeroUsize,
        new_flags: MemFlags,
    ) -> Result<UserVmRegion, UserVmRegionError> {
        let size_bytes = size_bytes.get();
        if !size_bytes.is_multiple_of(PAGE_SIZE) {
            return Err(UserVmRegionError::UnalignedSize);
        }
        let pages = size_bytes / PAGE_SIZE;
        let region = self
            .regions
            .iter_mut()
            .find(|r| r.matches(base, pages))
            .ok_or(UserVmRegionError::NotFound)?;
        region.flags = new_flags;
        Ok(*region)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const REGION_START: usize = 0x4000_0000;
    const REGION_END: usize = 0x8000_0000;

    fn allocator() -> UserVmAllocator {
        UserVmAllocator::new(
            PageAlignedVirtualAddress::from_usize(REGION_START).unwrap(),
            VirtualAddress::new(REGION_END),
        )
    }

    fn nz(value: usize) -> NonZeroUsize {
        NonZeroUsize::new(value).expect("test value must be non-zero")
    }

    fn assert_user_rw(flags: MemFlags) {
        match flags {
            MemFlags::Private(p) => {
                assert!(matches!(
                    p.user.access,
                    crate::mem_flags::AccessMode::Writable
                ));
            }
            MemFlags::Device(_) => panic!("expected Private flags"),
        }
    }

    fn assert_user_ro(flags: MemFlags) {
        match flags {
            MemFlags::Private(p) => {
                assert!(matches!(
                    p.user.access,
                    crate::mem_flags::AccessMode::Readonly
                ));
            }
            MemFlags::Device(_) => panic!("expected Private flags"),
        }
    }

    #[test]
    fn allocate_returns_consecutive_regions() {
        let mut a = allocator();

        let r1 = a.allocate(nz(PAGE_SIZE), MemFlags::user_rw()).unwrap();
        let r2 = a.allocate(nz(2 * PAGE_SIZE), MemFlags::user_rw()).unwrap();

        assert_eq!(r1.base().as_usize(), REGION_START);
        assert_eq!(r1.pages(), 1);
        assert_eq!(r2.base().as_usize(), REGION_START + PAGE_SIZE);
        assert_eq!(r2.pages(), 2);
        assert_user_rw(r1.flags());
        assert_user_rw(r2.flags());
    }

    #[test]
    fn allocate_unaligned_size_rejected() {
        let mut a = allocator();
        assert_eq!(
            a.allocate(nz(PAGE_SIZE + 1), MemFlags::user_rw()),
            Err(UserVmAllocateError::UnalignedSize)
        );
    }

    #[test]
    fn allocate_out_of_space() {
        let mut a = UserVmAllocator::new(
            PageAlignedVirtualAddress::from_usize(REGION_START).unwrap(),
            VirtualAddress::new(REGION_START + 2 * PAGE_SIZE),
        );
        a.allocate(nz(PAGE_SIZE), MemFlags::user_rw()).unwrap();
        a.allocate(nz(PAGE_SIZE), MemFlags::user_rw()).unwrap();
        assert_eq!(
            a.allocate(nz(PAGE_SIZE), MemFlags::user_rw()),
            Err(UserVmAllocateError::NotEnoughSpace)
        );
    }

    #[test]
    fn release_pending_rolls_back_last_allocation() {
        let mut a = allocator();
        let r1 = a.allocate(nz(PAGE_SIZE), MemFlags::user_rw()).unwrap();
        let r2 = a.allocate(nz(PAGE_SIZE), MemFlags::user_rw()).unwrap();
        assert!(a.release_pending(r2.base(), nz(r2.pages())));
        // mapper.map транзакционен -> освобождённый VA реюзится.
        let r3 = a.allocate(nz(PAGE_SIZE), MemFlags::user_rw()).unwrap();
        assert_eq!(r3.base(), r2.base());
        assert_eq!(a.live_count(), 2);
        let _ = r1;
    }

    /// Эмулирует точную последовательность `sys_memory_allocate` на пути OOM:
    /// `allocate` -> mapper.map -> Err -> `release_pending`. Поскольку
    /// `mapper.map` транзакционен, аллокатор обязан вернуться ровно в
    /// исходное состояние - следующее выделение приходит на тот же VA.
    #[test]
    fn syscall_oom_path_returns_allocator_to_initial_state() {
        let mut a = allocator();
        let initial_next = a.next_va;

        let attempted = a.allocate(nz(4 * PAGE_SIZE), MemFlags::user_rw()).unwrap();
        // Имитируем фейл транзакционного `mapper.map` (страницы не остались в page-tables).
        assert!(a.release_pending(attempted.base(), nz(attempted.pages())));

        assert_eq!(a.live_count(), 0);
        assert_eq!(a.next_va, initial_next);
        let next = a.allocate(nz(PAGE_SIZE), MemFlags::user_rw()).unwrap();
        assert_eq!(next.base(), attempted.base());
    }

    /// Откат "не последнего" региона не двигает bump: между ним и вершиной
    /// успели появиться другие `allocate`. Полноценное освобождение требует
    /// `vm_free` (отдельная задача).
    #[test]
    fn release_pending_non_top_region_keeps_bump() {
        let mut a = allocator();
        let r1 = a.allocate(nz(PAGE_SIZE), MemFlags::user_rw()).unwrap();
        let r2 = a.allocate(nz(PAGE_SIZE), MemFlags::user_rw()).unwrap();
        let bump_before = a.next_va;
        // r1 не на вершине - bump остаётся.
        assert!(a.release_pending(r1.base(), nz(r1.pages())));
        assert_eq!(a.next_va, bump_before);
        let r3 = a.allocate(nz(PAGE_SIZE), MemFlags::user_rw()).unwrap();
        assert_eq!(r3.base().as_usize(), r2.end_va().as_usize());
    }

    #[test]
    fn release_pending_unknown_region_returns_false() {
        let mut a = allocator();
        let bogus = PageAlignedVirtualAddress::from_usize(REGION_START + 16 * PAGE_SIZE).unwrap();
        assert!(!a.release_pending(bogus, nz(1)));
    }

    #[test]
    fn set_flags_updates_existing_region() {
        let mut a = allocator();
        let r = a.allocate(nz(2 * PAGE_SIZE), MemFlags::user_rw()).unwrap();
        let updated = a
            .set_flags(r.base(), nz(r.size_bytes()), MemFlags::user_ro())
            .unwrap();
        assert_user_ro(updated.flags());
        // Реестр действительно обновился.
        let reread = a.regions()[0];
        assert_user_ro(reread.flags());
    }

    #[test]
    fn set_flags_rejects_unaligned_size() {
        let mut a = allocator();
        let r = a.allocate(nz(PAGE_SIZE), MemFlags::user_rw()).unwrap();
        assert_eq!(
            a.set_flags(r.base(), nz(PAGE_SIZE + 1), MemFlags::user_ro()),
            Err(UserVmRegionError::UnalignedSize)
        );
    }

    #[test]
    fn set_flags_rejects_partial_match() {
        let mut a = allocator();
        let r = a.allocate(nz(2 * PAGE_SIZE), MemFlags::user_rw()).unwrap();
        // Корректный base, но другой размер - не совпадает.
        assert_eq!(
            a.set_flags(r.base(), nz(PAGE_SIZE), MemFlags::user_ro()),
            Err(UserVmRegionError::NotFound)
        );
    }

    #[test]
    fn set_flags_unknown_region_returns_not_found() {
        let mut a = allocator();
        let bogus = PageAlignedVirtualAddress::from_usize(REGION_START + 8 * PAGE_SIZE).unwrap();
        assert_eq!(
            a.set_flags(bogus, nz(PAGE_SIZE), MemFlags::user_ro()),
            Err(UserVmRegionError::NotFound)
        );
    }

    #[test]
    fn lookup_succeeds_for_existing_region() {
        let mut a = allocator();
        let r = a.allocate(nz(2 * PAGE_SIZE), MemFlags::user_rw()).unwrap();
        assert_eq!(a.lookup(r.base(), nz(r.size_bytes())), Ok(()));
    }

    #[test]
    fn lookup_rejects_unknown_region() {
        let a = allocator();
        let bogus = PageAlignedVirtualAddress::from_usize(REGION_START + 8 * PAGE_SIZE).unwrap();
        assert_eq!(
            a.lookup(bogus, nz(PAGE_SIZE)),
            Err(UserVmRegionError::NotFound)
        );
    }

    #[test]
    fn lookup_rejects_unaligned_size() {
        let mut a = allocator();
        let r = a.allocate(nz(PAGE_SIZE), MemFlags::user_rw()).unwrap();
        assert_eq!(
            a.lookup(r.base(), nz(PAGE_SIZE + 1)),
            Err(UserVmRegionError::UnalignedSize)
        );
    }

    #[test]
    fn out_of_slots_when_capacity_reached() {
        let mut a = UserVmAllocator::with_capacity(
            PageAlignedVirtualAddress::from_usize(REGION_START).unwrap(),
            VirtualAddress::new(REGION_END),
            2,
        );
        a.allocate(nz(PAGE_SIZE), MemFlags::user_rw()).unwrap();
        a.allocate(nz(PAGE_SIZE), MemFlags::user_rw()).unwrap();
        assert_eq!(
            a.allocate(nz(PAGE_SIZE), MemFlags::user_rw()),
            Err(UserVmAllocateError::OutOfSlots)
        );
    }
}
