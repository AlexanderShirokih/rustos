//! Маппинг виртуальных адресов на физические для AArch64.

use core::sync::atomic::{AtomicU64, Ordering};

use collections::LockCell;
use hal_aarch64_paging::{
    entry::{Entry, Invalid as InvalidEntry},
    level::{L0, L1, L2, L3, Level},
    mapper::{
        self, MapError, MapLeaf, PageMapper, UnmapError, UnmapVisitor, WalkError, extract_table_pa,
        free_all, unmap_range,
    },
    mem_flags::Aarch64MemFlags,
    page_table::PageTable,
    table_alloc::TableAlloc,
};
use memory::{
    MemFlags,
    aligned::Aligned,
    frame::Frame,
    frame_allocator::FrameAllocator,
    memory_mapper::{
        AddressSpaceHandle, AddressSpaceTag, MemoryMapper, MemoryMappingError,
        MemoryRemappingError, MemoryUnmappingError, UserCopyError,
    },
    physical_address::{AlignedPhysicalAddress, PageAlignedAddress, PhysicalAddress},
    virtual_address::{AlignedVirtualAddress, PageAlignedVirtualAddress, VirtualAddress},
};

use crate::memory::{
    asid::{self, unpack_asid},
    regs::{common::EL1, tlb::TranslationLookasideBuffer},
};

const OWNED_BY_FA_BIT: u64 = 1 << 55;

const AP_USER_BIT: u64 = 1 << 6;
const AP_RO_BIT: u64 = 1 << 7;

enum CopyDirection<'a> {
    In(&'a mut [u8]),
    Out(&'a [u8]),
}

fn leaf_user_accessible(raw: u64) -> bool {
    raw & AP_USER_BIT != 0
}

fn leaf_user_writable(raw: u64) -> bool {
    raw & AP_USER_BIT != 0 && raw & AP_RO_BIT == 0
}

pub(crate) struct FrameTableAlloc<'a, FA: FrameAllocator> {
    allocator: &'a FA,
    vaddr_offset: usize,
}

impl<'a, FA: FrameAllocator> FrameTableAlloc<'a, FA> {
    /// Создаёт аллокатор с линейным маппингом: VA = PA + offset.
    pub fn new(allocator: &'a FA, offset: usize) -> Self {
        Self {
            allocator,
            vaddr_offset: offset,
        }
    }
}

impl<FA: FrameAllocator> TableAlloc for FrameTableAlloc<'_, FA> {
    fn alloc_table_page(&mut self) -> Option<PageAlignedAddress> {
        let frame = self.allocator.allocate_frame()?;
        Some(frame.page_address())
    }

    unsafe fn unlinked_table_ptr<L: Level>(&self, pa: PageAlignedAddress) -> *mut PageTable<L> {
        // Линейная карта покрывает всю физическую память и не зависит от того,
        // связана ли страница с деревом трансляции.
        (pa.as_usize() + self.vaddr_offset) as *mut PageTable<L>
    }

    unsafe fn table_ptr<L: Level>(
        &self,
        pa: PageAlignedAddress,
        _target_va: usize,
    ) -> *mut PageTable<L> {
        // Линейное отображение таблиц страниц
        (pa.as_usize() + self.vaddr_offset) as *mut PageTable<L>
    }
}

/// Какому адресному пространству принадлежит маппер.
///
/// Различение влияет на `nG` leaf-страниц (kernel - global, user - per-ASID)
/// и на стратегию TLB-инвалидации в `remap`.
#[derive(Copy, Clone, Debug, Eq, PartialEq)]
pub enum AddressSpaceKind {
    Kernel,
    User,
}

/// Маппер памяти для AArch64.
pub struct Aarch64MemoryMapper<'a, FA, L>
where
    FA: FrameAllocator,
    L: LockCell<PageMapper<FrameTableAlloc<'a, FA>>>,
{
    frame_allocator: &'a FA,
    root_pa: PhysicalAddress,
    vaddr_offset: usize,
    mapper: L,
    asid_tag: AtomicU64,
    kind: AddressSpaceKind,
}

// SAFETY: доступ к page-tables сериализован `LockCell`, frame-аллокатор синхронизирует
// `&self`-методы внутри.
unsafe impl<'a, FA, L> Send for Aarch64MemoryMapper<'a, FA, L>
where
    FA: FrameAllocator,
    L: LockCell<PageMapper<FrameTableAlloc<'a, FA>>>,
{
}
// SAFETY: см. комментарий к `Send`.
unsafe impl<'a, FA, L> Sync for Aarch64MemoryMapper<'a, FA, L>
where
    FA: FrameAllocator,
    L: LockCell<PageMapper<FrameTableAlloc<'a, FA>>>,
{
}

impl<'a, FA, L> Aarch64MemoryMapper<'a, FA, L>
where
    FA: FrameAllocator,
    L: LockCell<PageMapper<FrameTableAlloc<'a, FA>>>,
{
    /// Создаёт маппер с заданным offset для трансляции PA->VA таблиц
    /// (`0` - identity mapping для boot до включения MMU).
    ///
    /// `kind` определяет роль AS: kernel-маппинги global, user-маппинги
    /// получают `nG=1` и per-AS ASID-тег на пути `activate_handle`.
    pub fn new_with_offset(
        frame_allocator: &'a FA,
        root_ptr: *mut PageTable<L0>,
        vaddr_offset: usize,
        kind: AddressSpaceKind,
    ) -> Self {
        // PA = VA - vaddr_offset. До MMU vaddr_offset=0 (identity), после MMU
        // - higher_half_base. Для свежевыделенных user-root таблиц передаётся
        // PA напрямую (см. конструктор фабрики).
        let root_pa = PhysicalAddress::new((root_ptr as usize).wrapping_sub(vaddr_offset));
        Self {
            frame_allocator,
            root_pa,
            vaddr_offset,
            mapper: L::new(PageMapper::new(
                root_ptr,
                FrameTableAlloc::new(frame_allocator, vaddr_offset),
            )),
            asid_tag: AtomicU64::new(0),
            kind,
        }
    }

    fn leaf_flags(&self, base: Aarch64MemFlags) -> Aarch64MemFlags {
        base.ng(matches!(self.kind, AddressSpaceKind::User))
    }

    /// Возвращает сырое значение leaf-дескриптора для `address`.
    ///
    /// `None` - если страница не замаплена либо лежит в block-mapping.
    /// Платформенный API для диагностики и инспекции таблиц трансляции
    /// (например, проверки состояния `nG`/`AP`/`AF` битов).
    #[allow(dead_code)]
    pub fn query_leaf_raw(&self, address: PageAlignedVirtualAddress) -> Option<u64> {
        self.mapper.with_lock(|mapper| {
            let (l3, idx) = mapper.walk_to_l3_leaf(address).ok()?;
            // SAFETY: walk_to_l3_leaf вернул валидный (l3, idx) для leaf-Page.
            Some(unsafe { (*l3).get_raw(idx) })
        })
    }

    pub fn map_exact_impl(
        &self,
        source_address: PageAlignedVirtualAddress,
        target_address: PageAlignedAddress,
        size: usize,
        mem_flags: Aarch64MemFlags,
    ) -> Result<(), MemoryMappingError> {
        if size == 0 {
            return Ok(());
        }

        let page_size = PageAlignedAddress::ALIGNMENT;
        let total_size = size.div_ceil(page_size) * page_size;
        let mem_flags = self.leaf_flags(mem_flags);

        self.mapper.with_lock(|mapper| {
            let mut remaining = total_size;
            let mut virt = source_address.as_virtual();
            let mut phys = target_address.as_physical_address();

            let l1_block = 1usize << L1::SHIFT;
            let l2_block = 1usize << L2::SHIFT;
            let l3_page = 1usize << L3::SHIFT;

            while remaining != 0 {
                if remaining >= l1_block
                    && let (Some(v1g), Some(p1g)) = (
                        AlignedVirtualAddress::<{ L1::SHIFT }>::new(virt),
                        AlignedPhysicalAddress::<{ L1::SHIFT }>::new(phys),
                    )
                {
                    map_contiguous_inner::<FA, { L1::SHIFT }, _>(mapper, v1g, p1g, mem_flags)?;
                    virt = virt.offset(l1_block);
                    phys = phys.add(l1_block);
                    remaining -= l1_block;
                    continue;
                }

                if remaining >= l2_block
                    && let (Some(v2m), Some(p2m)) = (
                        AlignedVirtualAddress::<{ L2::SHIFT }>::new(virt),
                        AlignedPhysicalAddress::<{ L2::SHIFT }>::new(phys),
                    )
                {
                    map_contiguous_inner::<FA, { L2::SHIFT }, _>(mapper, v2m, p2m, mem_flags)?;
                    virt = virt.offset(l2_block);
                    phys = phys.add(l2_block);
                    remaining -= l2_block;
                    continue;
                }

                let v4k = PageAlignedVirtualAddress::new_unchecked(virt);
                let p4k = PageAlignedAddress::new_unchecked(phys);
                map_contiguous_inner::<FA, { L3::SHIFT }, _>(mapper, v4k, p4k, mem_flags)?;
                virt = virt.offset(l3_page);
                phys = phys.add(l3_page);
                remaining -= l3_page;
            }

            // SAFETY: `dsb ish` - inner-shareable барьер без операндов, не модифицирует
            // регистры и не обращается к памяти напрямую (preserves_flags, nostack).
            unsafe {
                core::arch::asm!("dsb ish", options(nostack, preserves_flags));
            }

            Ok(())
        })
    }

    // SAFETY: `kernel_ptr` указывает на эксклюзивно владеемую 4К-выровненную
    // kernel-side mapping страницы.
    unsafe fn fill_frame_init(kernel_ptr: *mut u8, init: &[u8], seg_offset: usize) {
        let page_size = PageAlignedAddress::ALIGNMENT;
        // SAFETY: kernel_ptr валиден на FRAME_SIZE байт по контракту caller-а.
        unsafe {
            core::ptr::write_bytes(kernel_ptr, 0, page_size);
            if seg_offset < init.len() {
                let take = (init.len() - seg_offset).min(page_size);
                core::ptr::copy_nonoverlapping(init[seg_offset..].as_ptr(), kernel_ptr, take);
            }
        }
    }

    fn rollback_map(
        &self,
        mapper: &mut PageMapper<FrameTableAlloc<'a, FA>>,
        va: PageAlignedVirtualAddress,
        mapped_pages: usize,
    ) {
        // Промежуточные L1/L2/L3-таблицы этого же цикла остаются на месте. Пустая
        // таблица не меняет семантику маппинга и уходит при Drop AS.
        for i in 0..mapped_pages {
            let page_va = va.add_blocks(i);
            // Ошибка walker по только что замапленной странице означает, что
            // таблицы уже несогласованы - откат останавливается здесь.
            let Ok((l3, idx)) = mapper.walk_to_l3_leaf(page_va) else {
                break;
            };
            // SAFETY: walk_to_l3_leaf вернул валидный (l3, idx) для leaf-Page;
            // mapper-lock держится этим with_lock - эксклюзивный доступ.
            let raw = unsafe { (*l3).get_raw(idx) };
            // SAFETY: см. выше.
            unsafe { (*l3).set(idx, Entry::<L3, InvalidEntry>::invalid()) };
            if raw & OWNED_BY_FA_BIT != 0 {
                let pa = extract_table_pa(raw);
                let _ = self
                    .frame_allocator
                    .deallocate_frame(Frame::containing_address(pa.as_physical_address()));
            }
            self.invalidate_tlb_page(page_va.as_usize());
        }
    }

    fn invalidate_tlb_page(&self, va: usize) {
        match self.kind {
            AddressSpaceKind::User => {
                let asid = unpack_asid(self.asid_tag.load(Ordering::Acquire));
                // Нулевой тег означает AS, который ни разу не активировался, поэтому TLB не
                // содержит его трансляций.
                if asid != 0 {
                    TranslationLookasideBuffer::<EL1>::invalidate_va_asid(va, asid);
                }
            }
            AddressSpaceKind::Kernel => {
                TranslationLookasideBuffer::<EL1>::invalidate_va_global(va);
            }
        }
    }

    fn invalidate_tlb_leaf(&self, va: usize, leaf_shift: u8) {
        if leaf_shift == L1::SHIFT {
            match self.kind {
                AddressSpaceKind::User => {
                    let asid = unpack_asid(self.asid_tag.load(Ordering::Acquire));
                    if asid != 0 {
                        TranslationLookasideBuffer::<EL1>::invalidate_asid_inner_shareable(asid);
                    }
                }
                AddressSpaceKind::Kernel => {
                    TranslationLookasideBuffer::<EL1>::invalidate_global_inner_shareable();
                }
            }
            return;
        }
        let leaf_size = 1usize << leaf_shift;
        let mut offset = 0;
        while offset < leaf_size {
            self.invalidate_tlb_page(va + offset);
            offset += PageAlignedAddress::ALIGNMENT;
        }
    }

    fn dealloc_owned_leaf(&self, raw: u64) {
        // Leaf без software-бита пришёл через `map_exact`: его PA (MMIO, image,
        // identity) в RAM-bitmap не числится и в FA не возвращается.
        if raw & OWNED_BY_FA_BIT == 0 {
            return;
        }
        let pa = extract_table_pa(raw);
        let _ = self
            .frame_allocator
            .deallocate_frame(Frame::containing_address(pa.as_physical_address()));
    }

    fn dealloc_table_pa(&self, pa: PageAlignedAddress) {
        let _ = self
            .frame_allocator
            .deallocate_frame(Frame::containing_address(pa.as_physical_address()));
    }

    fn copy_user_bytes(
        &self,
        start: VirtualAddress,
        len: usize,
        mut dir: CopyDirection<'_>,
    ) -> Result<(), UserCopyError> {
        if len == 0 {
            return Ok(());
        }
        let page_size = PageAlignedAddress::ALIGNMENT;
        let need_writable = matches!(dir, CopyDirection::Out(_));

        // Все мутации таблиц идут под этим же lock, поэтому конкурентный unmap
        // с копированием не пересекается.
        self.mapper.with_lock(|mapper| {
            let mut copied = 0usize;
            while copied < len {
                let va = start.as_usize() + copied;
                let page_base = va & !(page_size - 1);
                let in_page_off = va - page_base;
                let take = (page_size - in_page_off).min(len - copied);

                let page_va = PageAlignedVirtualAddress::from_usize(page_base)
                    .ok_or(UserCopyError::NotMapped)?;

                let (l3, idx) = mapper
                    .walk_to_l3_leaf(page_va)
                    .map_err(|_| UserCopyError::NotMapped)?;
                // SAFETY: walk_to_l3_leaf вернул валидный (l3, idx) leaf-Page;
                // mapper-lock держится этим with_lock - таблицы стабильны.
                let raw = unsafe { (*l3).get_raw(idx) };

                if !leaf_user_accessible(raw) {
                    return Err(UserCopyError::AccessDenied);
                }
                if need_writable && !leaf_user_writable(raw) {
                    return Err(UserCopyError::AccessDenied);
                }

                let pa = extract_table_pa(raw);
                let kernel_ptr = (pa.as_usize() + self.vaddr_offset) as *mut u8;
                // SAFETY: kernel_ptr валиден на page_size байт по линейной
                // higher-half-карте; (in_page_off + take) <= page_size.
                unsafe {
                    let frame_byte = kernel_ptr.add(in_page_off);
                    match &mut dir {
                        CopyDirection::In(dst) => {
                            core::ptr::copy_nonoverlapping(
                                frame_byte,
                                dst.as_mut_ptr().add(copied),
                                take,
                            );
                        }
                        CopyDirection::Out(src) => {
                            core::ptr::copy_nonoverlapping(
                                src.as_ptr().add(copied),
                                frame_byte,
                                take,
                            );
                        }
                    }
                }
                copied += take;
            }
            Ok(())
        })
    }

    // SAFETY: `kernel_ptr` - валидный VA в линейной higher-half-карте.
    unsafe fn flush_icache_page(kernel_ptr: *mut u8) {
        let page_size = PageAlignedAddress::ALIGNMENT;
        // SAFETY: см. контракт caller-а.
        unsafe {
            let mut p = kernel_ptr;
            let end = kernel_ptr.add(page_size);
            while p < end {
                core::arch::asm!(
                    "ic ivau, {}",
                    in(reg) p,
                    options(nostack, preserves_flags),
                );
                // 64 байта - минимальный гарантированный размер I-cache line
                // на aarch64.
                p = p.add(64);
            }
        }
    }
}

fn map_contiguous_inner<FA: FrameAllocator, const SHIFT: u8, P>(
    mapper: &mut PageMapper<FrameTableAlloc<'_, FA>>,
    virt: AlignedVirtualAddress<SHIFT>,
    phys: P,
    mem_flags: Aarch64MemFlags,
) -> Result<(), MemoryMappingError>
where
    P: MapLeaf<SHIFT> + Into<PhysicalAddress>,
{
    match mapper.map_page(virt, phys, mem_flags) {
        Ok(()) => Ok(()),

        Err(MapError::NeedsSmallerPages) if SHIFT == L1::SHIFT => {
            let mut v = AlignedVirtualAddress::<{ L2::SHIFT }>::new_unchecked(virt.into());
            let mut p = AlignedPhysicalAddress::<{ L2::SHIFT }>::new_unchecked(phys.into());

            for _ in 0..512 {
                map_contiguous_inner::<FA, { L2::SHIFT }, _>(mapper, v, p, mem_flags)?;
                v = v.next_aligned();
                p = p.next_aligned();
            }
            Ok(())
        }

        Err(MapError::NeedsSmallerPages) if SHIFT == L2::SHIFT => {
            let mut v = AlignedVirtualAddress::<{ L3::SHIFT }>::new_unchecked(virt.into());
            let mut p = AlignedPhysicalAddress::<{ L3::SHIFT }>::new_unchecked(phys.into());

            for _ in 0..512 {
                map_contiguous_inner::<FA, { L3::SHIFT }, _>(mapper, v, p, mem_flags)?;
                v = v.next_aligned();
                p = p.next_aligned();
            }
            Ok(())
        }

        Err(MapError::AlreadyMapped) => Err(MemoryMappingError::AlreadyMapped),
        Err(MapError::OutOfMemory) => Err(MemoryMappingError::OutOfMemory),
        Err(_) => Err(MemoryMappingError::VirtualMappingError),
    }
}

struct UnmapPath<'m, 'a, FA, L>
where
    FA: FrameAllocator,
    L: LockCell<PageMapper<FrameTableAlloc<'a, FA>>>,
{
    mapper: &'m Aarch64MemoryMapper<'a, FA, L>,
}

impl<'a, FA, L> UnmapVisitor for UnmapPath<'_, 'a, FA, L>
where
    FA: FrameAllocator,
    L: LockCell<PageMapper<FrameTableAlloc<'a, FA>>>,
{
    fn on_leaf(&mut self, va: PageAlignedVirtualAddress, leaf_shift: u8, raw: u64) {
        self.mapper.invalidate_tlb_leaf(va.as_usize(), leaf_shift);
        self.mapper.dealloc_owned_leaf(raw);
    }
    fn on_free_table(&mut self, pa: PageAlignedAddress) {
        self.mapper.dealloc_table_pa(pa);
    }
}

struct DropPath<'a, FA: FrameAllocator> {
    frame_allocator: &'a FA,
}

impl<FA: FrameAllocator> UnmapVisitor for DropPath<'_, FA> {
    fn on_leaf(&mut self, _va: PageAlignedVirtualAddress, _leaf_shift: u8, raw: u64) {
        if raw & OWNED_BY_FA_BIT == 0 {
            return;
        }
        let pa = extract_table_pa(raw);
        let _ = self
            .frame_allocator
            .deallocate_frame(Frame::containing_address(pa.as_physical_address()));
    }
    fn on_free_table(&mut self, pa: PageAlignedAddress) {
        let _ = self
            .frame_allocator
            .deallocate_frame(Frame::containing_address(pa.as_physical_address()));
    }
}

impl<'a, FA, L> MemoryMapper for Aarch64MemoryMapper<'a, FA, L>
where
    'a: 'static,
    FA: FrameAllocator + 'static,
    L: LockCell<PageMapper<FrameTableAlloc<'a, FA>>> + 'static,
{
    fn map(
        &self,
        va: PageAlignedVirtualAddress,
        page_count: usize,
        init: &[u8],
        flags: MemFlags,
    ) -> Result<(), MemoryMappingError> {
        let page_size = PageAlignedAddress::ALIGNMENT;
        let mapped_size = page_count
            .checked_mul(page_size)
            .ok_or(MemoryMappingError::VirtualMappingError)?;
        va.as_usize()
            .checked_add(mapped_size)
            .ok_or(MemoryMappingError::VirtualMappingError)?;
        if init.len() > mapped_size {
            return Err(MemoryMappingError::InitExceedsMapping);
        }
        if page_count == 0 {
            return Ok(());
        }

        let user_exec = matches!(
            &flags,
            MemFlags::Cached(p) if matches!(
                p.user.executable,
                memory::mem_flags::Executable::Allowed
            )
        );
        let aarch64_flags = self.leaf_flags(Aarch64MemFlags::from_memflags(flags));

        self.mapper.with_lock(|mapper| {
            // Транзакционность: при partial-OOM (или любом другом фейле в
            // середине цикла) уже замапленные страницы 0..i откатываются -
            // PTE обнуляются, фреймы возвращаются аллокатору, TLB
            // инвалидируется. AS остаётся в исходном состоянии.
            for i in 0..page_count {
                let Some(frame) = self.frame_allocator.allocate_frame() else {
                    self.rollback_map(mapper, va, i);
                    return Err(MemoryMappingError::OutOfMemory);
                };
                let pa = frame.page_address();
                let kernel_ptr = (pa.as_usize() + self.vaddr_offset) as *mut u8;

                // SAFETY: фрейм свежевыделен и эксклюзивно наш; kernel_ptr через
                // линейную карту kernel-AS, выровнен на 4К.
                unsafe { Self::fill_frame_init(kernel_ptr, init, i * page_size) };

                let page_va = va.add_blocks(i);

                if let Err(e) =
                    map_contiguous_inner::<FA, { L3::SHIFT }, _>(mapper, page_va, pa, aarch64_flags)
                {
                    // Фрейм не попал в page-tables - возвращаем явно, затем
                    // откатываем предшествующие i маппингов.
                    let _ = self
                        .frame_allocator
                        .deallocate_frame(Frame::containing_address(pa.as_physical_address()));
                    self.rollback_map(mapper, va, i);
                    return Err(e);
                }

                // Бит 55 leaf-PTE отдан software по AArch64 ARM. Он помечает
                // фрейм, выданный нашим frame_allocator: только такой фрейм
                // `unmap` и Drop возвращают в FA.
                if let Ok((l3, idx)) = mapper.walk_to_l3_leaf(page_va) {
                    // SAFETY: walk_to_l3_leaf вернул валидный (l3, idx) leaf-Page;
                    // mapper-lock держится этим with_lock - эксклюзивный доступ.
                    unsafe {
                        let raw = (*l3).get_raw(idx);
                        (*l3).set_raw(idx, raw | OWNED_BY_FA_BIT);
                    }
                } else {
                    debug_assert!(false, "just-mapped leaf must walk to L3");
                }

                if user_exec {
                    // SAFETY: kernel_ptr - валидный VA в higher-half-карте.
                    unsafe { Self::flush_icache_page(kernel_ptr) };
                }
            }

            if user_exec {
                // SAFETY: `dsb ish`/`isb` - inner-shareable барьер и pipeline-синхронизация,
                // без операндов; не модифицируют регистры и не обращаются к памяти напрямую
                // (preserves_flags, nostack).
                unsafe {
                    core::arch::asm!("dsb ish", "isb", options(nostack, preserves_flags));
                }
            }

            Ok(())
        })
    }

    fn map_exact(
        &self,
        source_address: PageAlignedVirtualAddress,
        target_address: PageAlignedAddress,
        size: usize,
        mem_flags: MemFlags,
    ) -> Result<(), MemoryMappingError> {
        self.map_exact_impl(
            source_address,
            target_address,
            size,
            Aarch64MemFlags::from_memflags(mem_flags),
        )
    }

    fn unmap(
        &self,
        address: PageAlignedVirtualAddress,
        size: usize,
    ) -> Result<(), MemoryUnmappingError> {
        if size == 0 {
            return Ok(());
        }
        let page_size = PageAlignedVirtualAddress::ALIGNMENT;
        if !size.is_multiple_of(page_size) {
            return Err(MemoryUnmappingError::MisalignedRange);
        }

        self.mapper.with_lock(|mapper_inner| {
            let mut visitor = UnmapPath { mapper: self };
            // SAFETY: mapper-lock держится этим with_lock.
            unsafe {
                unmap_range(
                    mapper_inner.root_ptr(),
                    mapper_inner.alloc(),
                    address.as_usize(),
                    size,
                    &mut visitor,
                )
            }
            .map_err(|e| unmap_to_unmap_err(&e))
        })
    }

    fn activate_handle(&self) -> AddressSpaceHandle {
        match self.kind {
            AddressSpaceKind::Kernel => {
                AddressSpaceHandle::new(self.root_pa, AddressSpaceTag::NONE)
            }
            AddressSpaceKind::User => {
                let (_, raw) = asid::acquire(&self.asid_tag);
                AddressSpaceHandle::new(self.root_pa, AddressSpaceTag(raw))
            }
        }
    }

    fn zero_owned_frame(&self, pa: PageAlignedAddress) {
        let kernel_ptr = (pa.as_usize() + self.vaddr_offset) as *mut u8;
        // SAFETY: kernel-side линейная карта покрывает всю физическую память,
        // фрейм по `pa` существует и принадлежит caller (контракт
        // `MemoryRegion::Virtual`); 4К-выравнивание гарантировано типом `pa`.
        unsafe { core::ptr::write_bytes(kernel_ptr, 0, PageAlignedAddress::ALIGNMENT) };
    }

    fn copy_user_in(&self, va: VirtualAddress, dst: &mut [u8]) -> Result<(), UserCopyError> {
        self.copy_user_bytes(va, dst.len(), CopyDirection::In(dst))
    }

    fn copy_user_out(&self, va: VirtualAddress, src: &[u8]) -> Result<(), UserCopyError> {
        self.copy_user_bytes(va, src.len(), CopyDirection::Out(src))
    }

    fn as_any(&self) -> &(dyn core::any::Any + 'static) {
        self
    }

    fn remap(
        &self,
        start_address: PageAlignedVirtualAddress,
        size: usize,
        new_flags: MemFlags,
    ) -> Result<(), MemoryRemappingError> {
        if size == 0 {
            return Ok(());
        }
        let page_size = PageAlignedVirtualAddress::ALIGNMENT;
        if !size.is_multiple_of(page_size) {
            return Err(MemoryRemappingError::MisalignedRange);
        }
        let aarch64_flags = self.leaf_flags(Aarch64MemFlags::from_memflags(new_flags));
        let asid = unpack_asid(self.asid_tag.load(Ordering::Acquire));

        self.mapper.with_lock(|mapper| {
            let mut va = start_address.as_virtual();
            let mut left = size;
            while left != 0 {
                let page = PageAlignedVirtualAddress::new_unchecked(va);
                let (l3, idx) = mapper
                    .walk_to_l3_leaf(page)
                    .map_err(|e| walk_to_remap_err(&e))?;
                // SAFETY: walk_to_l3_leaf вернул валидный (l3, idx) для leaf-Page;
                // mapper-lock держится этим with_lock - эксклюзивный доступ к таблицам.
                unsafe { mapper::update_l3_flags(l3, idx, aarch64_flags) }
                    .map_err(|e| walk_to_remap_err(&e))?;

                match self.kind {
                    AddressSpaceKind::User => {
                        // Если AS ещё ни разу не активировался (asid == 0),
                        // TLB для него заведомо пуст - `tlbi` не нужен.
                        if asid != 0 {
                            TranslationLookasideBuffer::<EL1>::invalidate_va_asid(
                                page.as_usize(),
                                asid,
                            );
                        }
                    }
                    AddressSpaceKind::Kernel => {
                        // Kernel-маппинги global - инвалидируем VA во всех ASID.
                        TranslationLookasideBuffer::<EL1>::invalidate_va_global(page.as_usize());
                    }
                }

                va = va.offset(page_size);
                left -= page_size;
            }
            Ok::<(), MemoryRemappingError>(())
        })
    }
}

impl<'a, FA, L> Drop for Aarch64MemoryMapper<'a, FA, L>
where
    FA: FrameAllocator,
    L: LockCell<PageMapper<FrameTableAlloc<'a, FA>>>,
{
    fn drop(&mut self) {
        // Kernel-AS использует общий root и глобальные таблицы; их при Drop
        // не освобождаем.
        if !matches!(self.kind, AddressSpaceKind::User) {
            return;
        }
        let mut visitor = DropPath {
            frame_allocator: self.frame_allocator,
        };
        self.mapper.with_lock(|mapper_inner| {
            // SAFETY: AS больше не используется (scheduler переключил CPU до
            // cleanup), TTBR0 на root не указывает.
            unsafe {
                free_all(
                    mapper_inner.root_ptr(),
                    mapper_inner.alloc(),
                    /* high_half = */ false,
                    &mut visitor,
                );
            }
        });
        let _ = self
            .frame_allocator
            .deallocate_frame(Frame::containing_address(self.root_pa));
    }
}

fn walk_to_remap_err(err: &WalkError) -> MemoryRemappingError {
    match err {
        WalkError::HitBlock => MemoryRemappingError::UnsupportedBlockMapping,
        // Decode error в leaf-walk означает мусор в дескрипторе - для caller
        // практически неотличимо от Invalid: семантически "страница не замаплена".
        WalkError::NotMapped | WalkError::Decode(_) => MemoryRemappingError::NotMapped,
    }
}

fn unmap_to_unmap_err(err: &UnmapError) -> MemoryUnmappingError {
    match err {
        // Corrupt дескриптор для caller семантически = "не замаплено".
        UnmapError::NotMapped | UnmapError::Decode(_) => MemoryUnmappingError::NotMapped,
        UnmapError::UnsupportedPartialBlock => MemoryUnmappingError::UnsupportedBlockMapping,
    }
}
