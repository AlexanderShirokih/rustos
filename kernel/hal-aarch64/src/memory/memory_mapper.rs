//! Маппинг виртуальных адресов на физические для AArch64.

use core::sync::atomic::{AtomicU64, Ordering};

use hal_aarch64_paging::{
    level::{L0, L1, L2, L3, Level},
    linear_map::{LinearMap, LinearMapError},
    mapper::{
        self, MapError, MapLeaf, PageMapper, TableVisitor, UnmapError, WalkError, extract_table_pa,
        free_all, unmap_range,
    },
    mem_flags::Aarch64MemFlags,
    page_table::PageTable,
    table_alloc::TableAlloc,
    user_copy::{UserAccess, kernel_alias},
};
use kbase::LockCell;
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
    regs::{common::EL1, dcache::DataCache, tlb::TranslationLookasideBuffer},
};

const OWNED_BY_FA_BIT: u64 = 1 << 55;

const TLBI_BY_VA_MAX_PAGES: usize = 16;

pub(crate) const UNMAP_PENDING_FRAMES: usize = 64;
const UNMAP_MAX_FREED_TABLES: usize = 2 * 3;

const _: () = assert!(TLBI_BY_VA_MAX_PAGES + UNMAP_MAX_FREED_TABLES <= UNMAP_PENDING_FRAMES);

enum CopyDirection<'a> {
    In(&'a mut [u8]),
    Out(&'a [u8]),
}

pub(crate) struct FrameTableAlloc<'a, FA: FrameAllocator> {
    allocator: &'a FA,
    vaddr_offset: usize,
}

impl<'a, FA: FrameAllocator> FrameTableAlloc<'a, FA> {
    pub(crate) fn new(allocator: &'a FA, offset: usize) -> Self {
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
        // Линейная карта покрывает каждый фрейм аллокатора и не зависит от того,
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

#[derive(Copy, Clone)]
pub(crate) enum AddressSpaceKind {
    Kernel,
    User { linear_map: &'static LinearMap },
}

pub(crate) struct Aarch64MemoryMapper<'a, FA, L>
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
    pub(crate) fn new_with_offset(
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
        base.ng(matches!(self.kind, AddressSpaceKind::User { .. }))
    }

    fn clean_for_instruction_fetch(&self, pa: PageAlignedAddress, size: usize) {
        let start = pa.as_usize() + self.vaddr_offset;
        DataCache::<EL1>::clean_range_to_coherency(start, start + size);
    }

    #[allow(dead_code)]
    pub(crate) fn query_leaf_raw(&self, address: PageAlignedVirtualAddress) -> Option<u64> {
        self.mapper.with_lock(|mapper| {
            let (l3, idx) = mapper.walk_to_l3_leaf(address).ok()?;
            // SAFETY: walk_to_l3_leaf вернул валидный (l3, idx) для leaf-Page.
            Some(unsafe { (*l3).get_raw(idx) })
        })
    }

    pub(crate) fn map_exact_impl(
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

            publish_translation_tables();

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
        // Страницы `0..mapped_pages` замаплены этим же вызовом `map` под тем же lock.
        // Отказ обхода по ним означает, что таблицы уже несогласованы, и откат
        // останавливается на нём.
        let _ = self.unmap_locked(mapper, va, mapped_pages * PageAlignedAddress::ALIGNMENT);
    }

    fn unmap_locked(
        &self,
        mapper: &mut PageMapper<FrameTableAlloc<'a, FA>>,
        address: PageAlignedVirtualAddress,
        size: usize,
    ) -> Result<(), UnmapError> {
        let mut visitor = UnmapPath::new(self);
        let root = mapper.root_ptr();
        // SAFETY: caller держит mapper-lock, `root` - корень этого AS.
        let result =
            unsafe { unmap_range(root, mapper.alloc(), address.as_usize(), size, &mut visitor) };
        // Отказ на середине оставляет снятым префикс диапазона, поэтому инвалидация и
        // возврат фреймов выполняются и при ошибке.
        if visitor.cleared {
            self.invalidate_tlb_range(address.as_usize(), size);
            visitor.release();
        }
        result
    }

    fn invalidate_tlb_range(&self, va: usize, size: usize) {
        let pages = size.div_ceil(PageAlignedAddress::ALIGNMENT);
        if pages == 0 {
            return;
        }
        // Каждый широковещательный `tlbi` синхронизирует все CPU домена, а сброс ASID
        // заставляет заново заполнять TLB всего адресного пространства. Под QEMU на 8 CPU
        // снятие до 16 страниц по VA обходится не дороже сброса ASID, с 64 страниц дороже в
        // разы. Диапазон не длиннее порога освобождает не больше двух таблиц на каждом уровне
        // ниже корня, и его кадры помещаются в буфер `UnmapPath` без промежуточного сброса.
        if pages > TLBI_BY_VA_MAX_PAGES {
            self.invalidate_tlb_all();
            return;
        }
        match self.kind {
            AddressSpaceKind::User { .. } => {
                let asid = self.user_asid_after_clear();
                if asid != 0 {
                    TranslationLookasideBuffer::<EL1>::invalidate_va_range_asid(va, pages, asid);
                }
            }
            AddressSpaceKind::Kernel => {
                TranslationLookasideBuffer::<EL1>::invalidate_va_range_global(va, pages);
            }
        }
    }

    fn invalidate_tlb_all(&self) {
        match self.kind {
            AddressSpaceKind::User { .. } => {
                let asid = self.user_asid_after_clear();
                if asid != 0 {
                    TranslationLookasideBuffer::<EL1>::invalidate_asid_inner_shareable(asid);
                }
            }
            // Kernel-маппинги global: `aside1is` их не снимает.
            AddressSpaceKind::Kernel => {
                TranslationLookasideBuffer::<EL1>::invalidate_global_inner_shareable();
            }
        }
    }

    fn user_asid_after_clear(&self) -> u16 {
        // Пара с барьером `activate_handle`. Обнулённые записи видимы до чтения тега. Тег 0
        // означает, что AS ещё не активирован, и его первый обход таблиц читает уже
        // обнулённые записи. Ненулевой тег инвалидируется вызывающим.
        publish_translation_tables();
        unpack_asid(self.asid_tag.load(Ordering::Acquire))
    }

    fn deallocate(&self, pa: PageAlignedAddress) {
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
        // Kernel-AS не отображает страниц EL0.
        let AddressSpaceKind::User { linear_map } = self.kind else {
            return Err(UserCopyError::AccessDenied);
        };
        let page_size = PageAlignedAddress::ALIGNMENT;
        let access = match dir {
            CopyDirection::In(_) => UserAccess::Read,
            CopyDirection::Out(_) => UserAccess::Write,
        };

        // Все мутации таблиц идут под этим же lock, поэтому конкурентный unmap
        // с копированием не пересекается.
        self.mapper.with_lock(|mapper| {
            let mut copied = 0usize;
            while copied < len {
                let va = start.as_usize() + copied;
                let take = (page_size - va % page_size).min(len - copied);
                let kernel_ptr = kernel_alias(mapper, linear_map, VirtualAddress::new(va), access)?
                    .as_usize() as *mut u8;
                // SAFETY: `kernel_alias` подтвердил, что линейная карта содержит физическую
                // 4К-страницу пользователя и разрешает `access`. Оба отображения используют
                // одинаковые атрибуты кэширования. `MemorySetup` сохраняет снимок после
                // построения таблиц kernel-AS. Ядро снимает отображения только в окне MMIO
                // вне линейной карты.
                // `with_lock` защищает отображение пользователя от изменения. `take`
                // ограничивает копирование границами 4К-страницы, а `copied + take` не превышает
                // длину `dst` либо `src`.
                unsafe {
                    match &mut dir {
                        CopyDirection::In(dst) => {
                            core::ptr::copy_nonoverlapping(
                                kernel_ptr,
                                dst.as_mut_ptr().add(copied),
                                take,
                            );
                        }
                        CopyDirection::Out(src) => {
                            core::ptr::copy_nonoverlapping(
                                src.as_ptr().add(copied),
                                kernel_ptr,
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

    pub(crate) fn capture_linear_map(
        &self,
        base: PageAlignedVirtualAddress,
        size: usize,
    ) -> Result<LinearMap, LinearMapError> {
        self.mapper
            .with_lock(|kernel| LinearMap::capture(kernel, base, size))
    }
}

fn publish_translation_tables() {
    // Без `isb` обращение сразу после отображения может получить translation fault по
    // новой странице.
    // SAFETY: `dsb ish` и `isb` не имеют операндов и не модифицируют регистры.
    unsafe {
        core::arch::asm!("dsb ish", "isb", options(nostack, preserves_flags));
    }
}

fn is_user_executable(flags: MemFlags) -> bool {
    matches!(
        flags,
        MemFlags::Cached(p) if matches!(p.user.executable, memory::mem_flags::Executable::Allowed)
    )
}

fn invalidate_instruction_cache() {
    // На VIPT I-cache инвалидация по адресу линейной карты не снимает строк
    // пользовательского VA, поэтому I-cache инвалидируется целиком.
    // SAFETY: `ic ialluis`, `dsb ish` и `isb` не имеют операндов и не модифицируют
    // регистры.
    unsafe {
        core::arch::asm!(
            "ic ialluis",
            "dsb ish",
            "isb",
            options(nostack, preserves_flags)
        );
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
    pending: [PageAlignedAddress; UNMAP_PENDING_FRAMES],
    pending_len: usize,
    cleared: bool,
}

impl<'m, 'a, FA, L> UnmapPath<'m, 'a, FA, L>
where
    FA: FrameAllocator,
    L: LockCell<PageMapper<FrameTableAlloc<'a, FA>>>,
{
    fn new(mapper: &'m Aarch64MemoryMapper<'a, FA, L>) -> Self {
        Self {
            mapper,
            pending: [PageAlignedAddress::ZERO; UNMAP_PENDING_FRAMES],
            pending_len: 0,
            cleared: false,
        }
    }

    fn defer(&mut self, pa: PageAlignedAddress) {
        // До инвалидации TLB другой CPU обращается к фрейму по закэшированной трансляции, а
        // обходчик читает освобождённую таблицу. Буфер лежит на стеке ядра.
        if self.pending_len == UNMAP_PENDING_FRAMES {
            // Записи отложенных фреймов уже обнулены. Запись текущего фрейма
            // войдёт в инвалидацию диапазона после обхода.
            self.mapper.invalidate_tlb_all();
            self.release();
        }
        if let Some(slot) = self.pending.get_mut(self.pending_len) {
            *slot = pa;
            self.pending_len += 1;
        }
    }

    fn release(&mut self) {
        // Вызывающий уже выполнил инвалидацию, покрывающую записи отложенных фреймов.
        for &pa in self.pending.iter().take(self.pending_len) {
            self.mapper.deallocate(pa);
        }
        self.pending_len = 0;
    }
}

impl<'a, FA, L> TableVisitor for UnmapPath<'_, 'a, FA, L>
where
    FA: FrameAllocator,
    L: LockCell<PageMapper<FrameTableAlloc<'a, FA>>>,
{
    fn on_leaf(&mut self, _va: PageAlignedVirtualAddress, _leaf_shift: u8, raw: u64) {
        self.cleared = true;
        // Leaf без software-бита пришёл через `map_exact`: его PA (MMIO, image,
        // identity) в RAM-bitmap не числится и в FA не возвращается.
        if raw & OWNED_BY_FA_BIT != 0 {
            self.defer(extract_table_pa(raw));
        }
    }
    fn on_table(&mut self, pa: PageAlignedAddress) {
        // Таблица освобождается после инвалидации по VA своего диапазона: `tlbi` по VA
        // без суффикса последнего уровня снимает и закэшированные дескрипторы
        // промежуточных уровней. Диапазон освобождённой таблицы пересекается со
        // снимаемым.
        self.defer(pa);
    }
}

struct DropPath<'a, FA: FrameAllocator> {
    frame_allocator: &'a FA,
}

impl<FA: FrameAllocator> TableVisitor for DropPath<'_, FA> {
    fn on_leaf(&mut self, _va: PageAlignedVirtualAddress, _leaf_shift: u8, raw: u64) {
        if raw & OWNED_BY_FA_BIT == 0 {
            return;
        }
        let pa = extract_table_pa(raw);
        let _ = self
            .frame_allocator
            .deallocate_frame(Frame::containing_address(pa.as_physical_address()));
    }
    fn on_table(&mut self, pa: PageAlignedAddress) {
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

        let user_exec = is_user_executable(flags);
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
                    self.clean_for_instruction_fetch(pa, page_size);
                }
            }

            publish_translation_tables();

            if user_exec {
                invalidate_instruction_cache();
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
        let user_exec = is_user_executable(mem_flags);

        self.map_exact_impl(
            source_address,
            target_address,
            size,
            Aarch64MemFlags::from_memflags(mem_flags),
        )?;

        // Код во фреймах записан через другое отображение, например загрузчиком в userspace.
        if user_exec {
            self.clean_for_instruction_fetch(target_address, size);
            invalidate_instruction_cache();
        }

        Ok(())
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

        self.mapper.with_lock(|mapper| {
            self.unmap_locked(mapper, address, size)
                .map_err(|e| unmap_to_unmap_err(&e))
        })
    }

    fn activate_handle(&self) -> AddressSpaceHandle {
        match self.kind {
            AddressSpaceKind::Kernel => {
                AddressSpaceHandle::new(self.root_pa, AddressSpaceTag::NONE)
            }
            AddressSpaceKind::User { .. } => {
                let (_, raw) = asid::acquire(&self.asid_tag);
                // Пара с барьером `user_asid_after_clear`. Тег видим другим CPU до первого
                // обхода таблиц под ним.
                publish_translation_tables();
                AddressSpaceHandle::new(self.root_pa, AddressSpaceTag(raw))
            }
        }
    }

    fn zero_owned_frame(&self, pa: PageAlignedAddress) {
        let kernel_ptr = (pa.as_usize() + self.vaddr_offset) as *mut u8;
        // SAFETY: линейная карта ядра покрывает каждый фрейм аллокатора. Аллокатор выдал
        // фрейм по `pa` вызывающему коду (контракт `MemoryRegion::Virtual`). Тип `pa`
        // гарантирует выравнивание по 4К-странице.
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

        self.mapper.with_lock(|mapper| {
            let mut updated = 0;
            let mut result = Ok(());
            while updated != size {
                let page = PageAlignedVirtualAddress::new_unchecked(
                    start_address.as_virtual().offset(updated),
                );
                let update = mapper.walk_to_l3_leaf(page).and_then(|(l3, idx)| {
                    // SAFETY: walk_to_l3_leaf вернул валидный (l3, idx) для leaf-Page;
                    // mapper-lock держится этим with_lock - эксклюзивный доступ к таблицам.
                    unsafe { mapper::update_l3_flags(l3, idx, aarch64_flags) }
                });
                if let Err(e) = update {
                    result = Err(walk_to_remap_err(&e));
                    break;
                }
                updated += page_size;
            }
            // Отказ на середине оставляет обновлённым префикс диапазона.
            self.invalidate_tlb_range(start_address.as_usize(), updated);
            result
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
        if !matches!(self.kind, AddressSpaceKind::User { .. }) {
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
