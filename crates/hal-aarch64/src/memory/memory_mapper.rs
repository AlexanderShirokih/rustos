//! Маппинг виртуальных адресов на физические для AArch64.

use core::sync::atomic::{AtomicU64, Ordering};

use collections::LockCell;
use hal_aarch64_paging::{
    entry::{AnyEntry, Entry, decode},
    level::{L0, L1, L2, L3, Level},
    mapper::{self, MapError, MapLeaf, PageMapper, WalkError, extract_table_pa},
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
        MemoryRemappingError, MemoryUnmappingError,
    },
    physical_address::{AlignedPhysicalAddress, PageAlignedAddress, PhysicalAddress},
    virtual_address::{AlignedVirtualAddress, PageAlignedVirtualAddress},
};

use crate::memory::{
    asid::{self, unpack_asid},
    regs::{common::EL1, tlb::TranslationLookasideBuffer},
};

/// Адаптер FrameAllocator для выделения таблиц страниц.
///
/// Использует identity mapping (VA = PA) до включения MMU.
pub(crate) struct FrameTableAlloc<'a, FA: FrameAllocator> {
    /// Базовый аллокатор фреймов.
    allocator: &'a FA,
    /// Смещение для VA относительно PA (0 для identity).
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
    /// Аллокатор физических фреймов.
    frame_allocator: &'a FA,
    /// Физический адрес корня таблиц.
    root_pa: PhysicalAddress,
    /// Сдвиг линейного отображения PA->VA (`higher_half_base` в production,
    /// 0 для identity-mapping). Используется в `map` для kernel-side доступа
    /// к свежевыделенным фреймам при копировании `init`-байтов.
    vaddr_offset: usize,
    /// Внутренний маппер таблиц страниц.
    mapper: L,
    /// Текущий тег AS (`0` - никогда не активирован). Lazy-allocated на пути
    /// `activate_handle`.
    asid_tag: AtomicU64,
    /// Kernel или user - определяет `nG` leaf-страниц и стратегию TLB-flush.
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

            Ok(())
        })
    }

    /// VA-указатель на таблицу уровня `Lvl` через линейное PA->VA отображение
    /// (`pa + self.vaddr_offset`).
    ///
    /// # Safety
    /// `pa` должен быть валидным PA таблицы уровня `Lvl`, маппинг
    /// `pa -> pa + self.vaddr_offset` действителен в текущем CPU,
    /// и таблица не используется параллельно.
    unsafe fn table_ref<Lvl: Level>(&self, pa: PageAlignedAddress) -> &PageTable<Lvl> {
        let va = pa.as_usize() + self.vaddr_offset;
        // SAFETY: caller гарантирует валидность PA + линейную карту.
        unsafe { &*(va as *const PageTable<Lvl>) }
    }

    /// Обходит дерево таблиц от L0-root user-AS и возвращает в аллокатор:
    /// все user-data страницы (leaf-entries L3), все промежуточные таблицы
    /// (L1/L2/L3) и root.
    ///
    /// # Safety
    /// Вызывающий гарантирует, что:
    /// - PA таблиц валидны и доступны через линейную карту kernel-AS
    ///   (`pa + self.vaddr_offset`);
    /// - таблицы не используются параллельно (Drop AS = эксклюзивное владение);
    /// - block-mappings (L1 1G / L2 2M) в user-AS не встречаются (этот ядерный
    ///   путь маппит только L3 страницы).
    unsafe fn free_user_page_tables(&self) {
        // SAFETY: L0-root доступен через линейную карту; root_pa валиден на всё
        // время жизни AS (фрейм выделен фабрикой и не освобождается до Drop).
        let l0 = unsafe { self.table_ref::<L0>(PageAlignedAddress::new_unchecked(self.root_pa)) };
        let mut had_mappings = false;
        for idx in 0..512 {
            let raw = l0.get_raw(idx);
            if let Ok(AnyEntry::Table(te)) = decode::<L0>(raw) {
                had_mappings = true;
                let l1_pa = extract_table_pa(te.raw());
                // SAFETY: te - валидный Table-дескриптор уровня L0; l1_pa указывает на L1.
                unsafe { self.free_l1(l1_pa) };
                let _ = self
                    .frame_allocator
                    .deallocate_frame(Frame::containing_address(l1_pa.as_physical_address()));
            }
            // Invalid -> пропускаем; Block на L0 невозможен (CanBlock не реализован).
        }
        // Root освобождаем только если AS реально использовался (имел маппинги).
        // Пустой user-AS (в production не возникает - он создаётся фабрикой
        // только под `spawn_user_process`, который всегда маппит payload+стек)
        // встречается лишь в kernel-side юнит-кейсах вида `AddressSpace::new_user`
        // без последующего `map`. Возвращать его одинокий root в bitmap создаёт
        // изолированный "дырявый" бит - текущий heap-аллокатор после такого
        // expand'а не может найти большой aligned-блок и валится в OOM. Всё
        // равно root для реального user-process очищается в основной ветке.
        if had_mappings {
            let _ = self
                .frame_allocator
                .deallocate_frame(Frame::containing_address(self.root_pa));
        }
    }

    /// Освобождает все L2-таблицы под `l1_pa` и leaf L3-таблицы под ними.
    ///
    /// # Safety
    /// `l1_pa` должен быть PA валидной L1-таблицы; см. `free_user_page_tables`.
    unsafe fn free_l1(&self, l1_pa: PageAlignedAddress) {
        // SAFETY: l1_pa получен из валидного Table-дескриптора уровня L0.
        let l1 = unsafe { self.table_ref::<L1>(l1_pa) };
        for idx in 0..512 {
            let raw = l1.get_raw(idx);
            // Block (1G) на этом уровне в user-AS не создаётся (`map` маппит только L3),
            // фрейм под ним не наш. Invalid/Decode/Page - игнорируем.
            if let Ok(AnyEntry::Table(te)) = decode::<L1>(raw) {
                let l2_pa = extract_table_pa(te.raw());
                // SAFETY: l2_pa получен из валидного Table-дескриптора уровня L1.
                unsafe { self.free_l2(l2_pa) };
                let _ = self
                    .frame_allocator
                    .deallocate_frame(Frame::containing_address(l2_pa.as_physical_address()));
            }
        }
    }

    /// Освобождает все leaf-страницы под L3-таблицами этого L2 и сами L3-таблицы.
    ///
    /// # Safety
    /// `l2_pa` должен быть PA валидной L2-таблицы; см. `free_user_page_tables`.
    unsafe fn free_l2(&self, l2_pa: PageAlignedAddress) {
        // SAFETY: l2_pa получен из валидного Table-дескриптора уровня L1.
        let l2 = unsafe { self.table_ref::<L2>(l2_pa) };
        for idx in 0..512 {
            let raw = l2.get_raw(idx);
            // Block (2M) на этом уровне в user-AS не создаётся; Invalid/Decode - пропускаем.
            if let Ok(AnyEntry::Table(te)) = decode::<L2>(raw) {
                let l3_pa = extract_table_pa(te.raw());
                // SAFETY: l3_pa получен из валидного L2 Table-дескриптора.
                unsafe { self.free_l3_leaves(l3_pa) };
                let _ = self
                    .frame_allocator
                    .deallocate_frame(Frame::containing_address(l3_pa.as_physical_address()));
            }
        }
    }

    /// Освобождает все user-data страницы, на которые указывают entries L3-таблицы.
    ///
    /// # Safety
    /// `l3_pa` должен быть PA валидной L3-таблицы; см. `free_user_page_tables`.
    unsafe fn free_l3_leaves(&self, l3_pa: PageAlignedAddress) {
        // SAFETY: l3_pa валиден.
        let l3 = unsafe { self.table_ref::<L3>(l3_pa) };
        for idx in 0..512 {
            let raw = l3.get_raw(idx);
            // L3 page descriptor: биты [1:0] == 0b11 (Page), биты [47:12] = output PA.
            // `extract_table_pa` использует ту же маску - переиспользуем.
            if (raw & 0b11) == 0b11 {
                let page_pa = extract_table_pa(raw);
                let _ = self
                    .frame_allocator
                    .deallocate_frame(Frame::containing_address(page_pa.as_physical_address()));
            }
            // Invalid (0b00) - пропускаем. Block на L3 невозможен.
        }
    }

    /// Заполняет страницу через kernel-VA: зануляет её, затем копирует
    /// срез `init[seg_offset..]` максимум в FRAME_SIZE байт.
    ///
    /// # Safety
    /// `kernel_ptr` указывает на эксклюзивно-владеемую 4К-выровненную
    /// kernel-side mapping страницы.
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

    /// Software-bit в leaf-PTE (бит 55 - software-available по AArch64 ARM):
    /// установлен -> фрейм был аллоцирован `frame_allocator` этого mapper'а
    /// (путь [`MemoryMapper::map`]); сброшен -> PA пришёл от вызывающего
    /// (путь [`MemoryMapper::map_exact`] - MMIO, image, identity).
    /// `unmap`/`rollback_map` возвращают в FA только страницы с битом -
    /// MMIO/устройственные PA не попадают в bitmap RAM.
    const OWNED_BY_FA_BIT: u64 = 1 << 55;

    /// Откатывает первые `mapped_pages` страниц диапазона начиная с `va`:
    /// обнуляет PTE, возвращает фрейм аллокатору, инвалидирует TLB.
    ///
    /// Используется на пути транзакционного `map` при partial-OOM. Все
    /// страницы только что замаплены этим же циклом и помечены
    /// `OWNED_BY_FA_BIT`, поэтому walk_to_l3_leaf не может вернуть
    /// `HitBlock`/`NotMapped`, а deallocate безопасен.
    ///
    /// Промежуточные L1/L2/L3-таблицы, которые `map_contiguous_inner` мог
    /// создать в этом же цикле, **не** освобождаются - они освободятся при
    /// Drop AS. Это безопасно: пустые таблицы не нарушают семантику и не
    /// мешают повторному `map` по тем же VA.
    fn rollback_map(
        &self,
        mapper: &mut PageMapper<FrameTableAlloc<'a, FA>>,
        va: PageAlignedVirtualAddress,
        mapped_pages: usize,
    ) {
        let page_size = PageAlignedAddress::ALIGNMENT;
        for i in 0..mapped_pages {
            let page_va = PageAlignedVirtualAddress::from_usize(va.as_usize() + i * page_size)
                .expect("rollback page va remains 4K-aligned");
            // Должно быть невозможно: страница только что замаплена этим же
            // циклом. Если walker всё-таки вернул ошибку - дальнейший откат
            // невозможен; останавливаемся, чтобы не trail'ить ещё один
            // источник несогласованности.
            let Ok((l3, idx)) = mapper.walk_to_l3_leaf(page_va) else {
                break;
            };
            // SAFETY: walk_to_l3_leaf вернул валидный (l3, idx) для leaf-Page;
            // mapper-lock держится этим with_lock - эксклюзивный доступ.
            let (pa, owned) = unsafe { Self::take_l3_leaf(l3, idx) };
            if owned {
                let _ = self
                    .frame_allocator
                    .deallocate_frame(Frame::containing_address(pa.as_physical_address()));
            }
            self.invalidate_tlb_page(page_va.as_usize());
        }
    }

    /// Точечно инвалидирует TLB для одной 4К-страницы по `va`.
    ///
    /// Для user-AS использует `tlbi vae1is, (va | asid)`; если AS ни разу не
    /// активировался (`asid == 0`), TLB заведомо пуст и tlbi пропускается.
    /// Для kernel-AS - broadcast по всем ASID (`tlbi vaae1is`).
    fn invalidate_tlb_page(&self, va: usize) {
        match self.kind {
            AddressSpaceKind::User => {
                let asid = unpack_asid(self.asid_tag.load(Ordering::Acquire));
                if asid != 0 {
                    TranslationLookasideBuffer::<EL1>::invalidate_va_asid(va, asid);
                }
            }
            AddressSpaceKind::Kernel => {
                TranslationLookasideBuffer::<EL1>::invalidate_va_global(va);
            }
        }
    }

    /// Чистит leaf-PTE по `(l3, idx)`, возвращает `(PA, owned)`: PA - адрес
    /// фрейма из старой записи; `owned` - был ли установлен `OWNED_BY_FA_BIT`
    /// (т.е. фрейм когда-то аллоцирован через `frame_allocator`).
    ///
    /// Запись обнуляется. Фрейм **не** возвращается аллокатору - решение
    /// принимает вызывающий, ориентируясь на `owned`. TLB здесь тоже не
    /// трогается; вызывающий выполняет инвалидацию после изменения PTE.
    ///
    /// # Safety
    /// - `l3` валиден и эксклюзивно доступен (caller держит mapper-lock);
    /// - `idx < 512`;
    /// - запись по `idx` имеет desc-type Page (`0b11`).
    unsafe fn take_l3_leaf(table: *mut PageTable<L3>, idx: usize) -> (PageAlignedAddress, bool) {
        // SAFETY: см. контракт.
        let raw = unsafe { (*table).get_raw(idx) };
        let pa = extract_table_pa(raw);
        let owned = (raw & Self::OWNED_BY_FA_BIT) != 0;
        // SAFETY: см. контракт; запись Invalid инвалидирует leaf-дескриптор.
        unsafe {
            (*table).set(
                idx,
                Entry::<L3, hal_aarch64_paging::entry::Invalid>::invalid(),
            );
        }
        (pa, owned)
    }

    /// Per-line `ic ivau` по kernel-VA для свежезаписанной exec-страницы.
    /// Шаг 64 байта - минимальный гарантированный размер I-cache line на aarch64.
    ///
    /// # Safety
    /// `kernel_ptr` - валидный VA в линейной higher-half-карте.
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
        if page_count == 0 {
            return Ok(());
        }
        let page_size = PageAlignedAddress::ALIGNMENT;
        assert!(
            init.len() <= page_count * page_size,
            "init exceeds mapped size"
        );

        let user_exec = matches!(
            &flags,
            MemFlags::Private(p) if matches!(
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

                let page_va = PageAlignedVirtualAddress::from_usize(va.as_usize() + i * page_size)
                    .expect("4K * i + aligned base remains 4K-aligned");

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

                // Помечаем leaf SW-битом OWNED_BY_FA_BIT: фрейм аллоцирован
                // нашим frame_allocator-ом - его можно вернуть в FA при unmap.
                // map_exact-leaves остаются без бита и не освобождаются.
                let (l3, idx) = mapper
                    .walk_to_l3_leaf(page_va)
                    .expect("just-mapped leaf must walk to L3");
                // SAFETY: walk_to_l3_leaf вернул валидный (l3, idx) leaf-Page;
                // mapper-lock держится этим with_lock - эксклюзивный доступ.
                unsafe {
                    let raw = (*l3).get_raw(idx);
                    (*l3).set_raw(idx, raw | Self::OWNED_BY_FA_BIT);
                }

                if user_exec {
                    // SAFETY: kernel_ptr - валидный VA в higher-half-карте.
                    unsafe { Self::flush_icache_page(kernel_ptr) };
                }
            }

            if user_exec {
                // SAFETY: barrier-only.
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

        // TODO: освобождать пустые промежуточные L1/L2/L3-таблицы, когда
        // последняя их leaf-запись уходит. Пока они утекают до Drop AS - это
        // безопасно, но даёт неоптимальный peak-footprint.
        self.mapper.with_lock(|mapper| {
            let mut va = address.as_virtual();
            let mut left = size;
            while left != 0 {
                let page = PageAlignedVirtualAddress::new_unchecked(va);
                let (l3, idx) = mapper
                    .walk_to_l3_leaf(page)
                    .map_err(|e| walk_to_unmap_err(&e))?;
                // SAFETY: walk_to_l3_leaf вернул валидный (l3, idx) для leaf-Page;
                // mapper-lock держится этим with_lock - эксклюзивный доступ.
                let (pa, owned) = unsafe { Self::take_l3_leaf(l3, idx) };
                // map_exact-страницы (caller-supplied PA - MMIO, image, identity)
                // не возвращаем во FrameAllocator: PA не принадлежит RAM-bitmap'у,
                // deallocate либо корруптит bitmap, либо тихо ошибётся.
                if owned {
                    let _ = self
                        .frame_allocator
                        .deallocate_frame(Frame::containing_address(pa.as_physical_address()));
                }
                self.invalidate_tlb_page(page.as_usize());
                va = va.offset(page_size);
                left -= page_size;
            }
            Ok::<(), MemoryUnmappingError>(())
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
        // Page-table walk для user-AS: рекурсивно обходим L0..L3 и возвращаем
        // в frame-аллокатор все user-data страницы (leaf-entries L3) +
        // intermediate tables (L1/L2/L3) + root.
        // Kernel-AS использует общий root и таблицы, выделенные глобально, -
        // освобождать их при Drop kernel-mapper-а нельзя.
        if matches!(self.kind, AddressSpaceKind::User) {
            // SAFETY: Drop вызывается, когда последний владелец AS сброшен -
            // ни один поток не использует эти таблицы. TTBR0 на этот root
            // больше не указывает (scheduler переключил CPU до cleanup-а),
            // `vaddr_offset` валиден на всё время жизни ядра.
            unsafe { self.free_user_page_tables() };
        }
    }
}

fn walk_to_remap_err(err: &WalkError) -> MemoryRemappingError {
    match err {
        WalkError::HitBlock => MemoryRemappingError::UnsupportedBlockMapping,
        // Decode error в leaf-walk означает мусор в дескрипторе - для caller'а
        // практически неотличимо от Invalid: семантически "страница не замаплена".
        WalkError::NotMapped | WalkError::Decode(_) => MemoryRemappingError::NotMapped,
    }
}

fn walk_to_unmap_err(err: &WalkError) -> MemoryUnmappingError {
    match err {
        WalkError::HitBlock => MemoryUnmappingError::UnsupportedBlockMapping,
        WalkError::NotMapped | WalkError::Decode(_) => MemoryUnmappingError::NotMapped,
    }
}
