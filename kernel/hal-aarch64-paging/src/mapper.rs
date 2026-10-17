//! Маппинг виртуальных адресов на физические.

use memory::{
    aligned::Aligned,
    physical_address::{PageAlignedAddress, PhysicalAddress},
    virtual_address::{AlignedVirtualAddress, PageAlignedVirtualAddress, VirtualAddress},
};

use crate::{
    entry::{
        AnyEntry, Block, CanTable, DecodeBlock, DecodeError, Entry, Invalid, Page, Table, decode,
    },
    level::{L0, L1, L1BlockPa, L2, L2BlockPa, L3, Level, PagePa},
    mem_flags::Aarch64MemFlags,
    page_table::PageTable,
    table_alloc::TableAlloc,
    table_flags::TableFlags,
    virtual_address::VirtualAddressExt,
};

const OUTPUT_ADDRESS_MASK: u64 = 0x0000_FFFF_FFFF_F000;
const DESC_TYPE_MASK: u64 = 0b11;

/// Физический адрес, который можно замапить на виртуальный.
pub trait MapLeaf<const SHIFT: u8>: Copy {
    fn map_into<A: TableAlloc>(
        mapper: &mut PageMapper<A>,
        virt: AlignedVirtualAddress<SHIFT>,
        phys: Self,
        flags: Aarch64MemFlags,
    ) -> Result<(), MapError>;
}

impl MapLeaf<{ L3::SHIFT }> for PagePa {
    fn map_into<A: TableAlloc>(
        mapper: &mut PageMapper<A>,
        virt: PageAlignedVirtualAddress,
        phys: Self,
        flags: Aarch64MemFlags,
    ) -> Result<(), MapError> {
        let target_va = virt.as_usize();
        let l0 = mapper.l0_ptr();
        let l1 = mapper.ensure_next::<L0, L1>(l0, target_va)?;
        let l2 = mapper.ensure_next::<L1, L2>(l1, target_va)?;
        let l3 = mapper.ensure_next::<L2, L3>(l2, target_va)?;
        let idx = virt.index::<L3>();

        // SAFETY: l3 валиден после ensure_next
        let raw = unsafe { (*l3).get_raw(idx) };
        if raw != 0 {
            return Err(MapError::AlreadyMapped);
        }
        // SAFETY: см. выше - l3 валиден после ensure_next, idx в пределах 0..512.
        unsafe { (*l3).set(idx, Entry::<L3, Page>::new(phys, flags)) };
        Ok(())
    }
}

impl MapLeaf<{ L2::SHIFT }> for L2BlockPa {
    fn map_into<A: TableAlloc>(
        mapper: &mut PageMapper<A>,
        virt: AlignedVirtualAddress<{ L2::SHIFT }>,
        phys: Self,
        flags: Aarch64MemFlags,
    ) -> Result<(), MapError> {
        let target_va = virt.as_usize();
        let l0 = mapper.l0_ptr();
        let l1 = mapper.ensure_next::<L0, L1>(l0, target_va)?;
        let l2 = mapper.ensure_next::<L1, L2>(l1, target_va)?;
        let idx = virt.index::<L2>();

        // SAFETY: l2 валиден после ensure_next
        let raw = unsafe { (*l2).get_raw(idx) };
        match decode::<L2>(raw).map_err(MapError::Decode)? {
            AnyEntry::Invalid(_) => {
                // SAFETY: см. выше - l2 валиден после ensure_next, idx в пределах 0..512.
                unsafe { (*l2).set(idx, Entry::<L2, Block>::new(phys, flags)) };
                Ok(())
            }
            AnyEntry::Table(_) => Err(MapError::NeedsSmallerPages),
            AnyEntry::Block(_) | AnyEntry::Page(_) => Err(MapError::AlreadyMapped),
        }
    }
}

impl MapLeaf<{ L1::SHIFT }> for L1BlockPa {
    fn map_into<A: TableAlloc>(
        mapper: &mut PageMapper<A>,
        virt: AlignedVirtualAddress<{ L1::SHIFT }>,
        phys: Self,
        flags: Aarch64MemFlags,
    ) -> Result<(), MapError> {
        let target_va = virt.as_usize();
        let l0 = mapper.l0_ptr();
        let l1 = mapper.ensure_next::<L0, L1>(l0, target_va)?;
        let idx = virt.index::<L1>();

        // SAFETY: l1 валиден после ensure_next
        let raw = unsafe { (*l1).get_raw(idx) };
        match decode::<L1>(raw).map_err(MapError::Decode)? {
            AnyEntry::Invalid(_) => {
                // SAFETY: см. выше - l1 валиден после ensure_next, idx в пределах 0..512.
                unsafe { (*l1).set(idx, Entry::<L1, Block>::new(phys, flags)) };
                Ok(())
            }
            AnyEntry::Table(_) => Err(MapError::NeedsSmallerPages),
            AnyEntry::Block(_) | AnyEntry::Page(_) => Err(MapError::AlreadyMapped),
        }
    }
}

/// Маппер виртуальных адресов.
///
/// Управляет иерархией таблиц страниц и создаёт маппинги.
pub struct PageMapper<A: TableAlloc> {
    root: *mut PageTable<L0>,
    alloc: A,
    table_flags: TableFlags,
}

impl<A: TableAlloc> PageMapper<A> {
    /// Создаёт маппер с заданной корневой таблицей и аллокатором.
    ///
    /// `root` указывает на таблицу уровня L0, доступную по этому адресу. Она должна
    /// оставаться действительной на всё время жизни `PageMapper` - последующие вызовы
    /// разыменовывают её напрямую.
    ///
    /// Table-флаги (PXNTable/UXNTable/APTable) остаются нулевыми. Эти биты - иерархическое
    /// ужесточение и только запрещают: UXNTable=1 делает любую leaf-страницу ниже
    /// non-executable для EL0 даже при UXN=0 в leaf-дескрипторе, и это ломает
    /// userspace-маппинги, у которых leaf явно UXN=0 (UserRX). Kernel-only страницы защищают
    /// leaf-флаги. `Heap::flags`, `KernelData::flags`, `KernelRoData::flags` и прочие уже
    /// выставляют `uxn(true)`.
    pub fn new(root: *mut PageTable<L0>, alloc: A) -> Self {
        Self {
            root,
            alloc,
            table_flags: TableFlags::new(),
        }
    }

    #[inline]
    fn l0_ptr(&self) -> *mut PageTable<L0> {
        self.root
    }

    #[inline]
    pub fn root_ptr(&self) -> *mut PageTable<L0> {
        self.root
    }

    #[inline]
    pub fn alloc(&self) -> &A {
        &self.alloc
    }

    /// Создаёт маппинг виртуального адреса на физический.
    ///
    /// Уже занятая запись (Table/Block/Page) не заменяется. Вызов на такую запись возвращает
    /// [`MapError::AlreadyMapped`]. TLB не инвалидируется.
    pub fn map_page<const SHIFT: u8, P: MapLeaf<SHIFT>>(
        &mut self,
        virt: AlignedVirtualAddress<SHIFT>,
        phys: P,
        flags: Aarch64MemFlags,
    ) -> Result<(), MapError> {
        P::map_into(self, virt, phys, flags)
    }

    fn ensure_next<PL, CL>(
        &mut self,
        parent: *mut PageTable<PL>,
        target_va: usize,
    ) -> Result<*mut PageTable<CL>, MapError>
    where
        PL: Level + CanTable + DecodeBlock,
        CL: Level,
    {
        let idx = (target_va >> PL::SHIFT) & 0x1FF;

        // SAFETY: parent получен из self.root или предыдущего вызова ensure_next
        let raw = unsafe { (*parent).get_raw(idx) };

        match decode::<PL>(raw).map_err(MapError::Decode)? {
            AnyEntry::Table(te) => {
                // Таблица существует - извлекаем PA и получаем указатель
                let child_pa = extract_table_pa(te.raw());
                // SAFETY: child_pa извлечён из валидного дескриптора уровня PL и указывает
                // на ранее замапленную таблицу уровня CL; alloc.table_ptr транслирует PA->VA
                // согласно своему контракту (recursive/linear mapping).
                let child = unsafe { self.alloc.table_ptr::<CL>(child_pa, target_va) };
                Ok(child)
            }

            AnyEntry::Invalid(_) => {
                let child_pa = self.alloc.alloc_table_page().ok_or(MapError::OutOfMemory)?;

                // SAFETY: child_pa только что выдан alloc-ом, не связан с деревом
                // и не имеет другого владельца - предусловие unlinked_table_ptr.
                let unlinked = unsafe { self.alloc.unlinked_table_ptr::<CL>(child_pa) };
                // SAFETY: страница эксклюзивно принадлежит mapper-у до связывания;
                // запись пустой PageTable инициализирует все 512 entries в Invalid.
                unsafe { unlinked.write(PageTable::new()) };

                // Опубликованная в родителе дочерняя таблица содержит только
                // корректно инициализированные дескрипторы.
                // SAFETY: parent валиден (см. ensure_next caller), idx в пределах 0..512;
                // запись Table-entry делает достижимой инициализированную страницу.
                unsafe { (*parent).set(idx, Entry::<PL, Table>::new(child_pa, self.table_flags)) };

                // SAFETY: таблица по child_pa инициализирована и достижима через
                // parent[idx] - предусловие table_ptr выполнено.
                Ok(unsafe { self.alloc.table_ptr::<CL>(child_pa, target_va) })
            }

            AnyEntry::Block(_) | AnyEntry::Page(_) => Err(MapError::AlreadyMapped),
        }
    }
}

/// Извлекает физический адрес таблицы из дескриптора.
pub fn extract_table_pa(raw: u64) -> PageAlignedAddress {
    PageAlignedAddress::new_unchecked(PhysicalAddress::new((raw & OUTPUT_ADDRESS_MASK) as usize))
}

/// Ошибка маппинга.
#[derive(Debug)]
pub enum MapError {
    /// Не удалось выделить память под таблицу.
    OutOfMemory,
    /// В ячейке уже есть таблица - нужны страницы меньшего размера.
    NeedsSmallerPages,
    /// Адрес уже замаплен.
    AlreadyMapped,
    /// Ошибка декодирования записи.
    Decode(DecodeError),
}

/// Ошибка walk/update leaf-записи.
#[derive(Debug, Eq, PartialEq)]
pub enum WalkError {
    /// На каком-то уровне найден Invalid-дескриптор.
    NotMapped,
    /// На пути встретился block-mapping (L1=1G или L2=2M).
    HitBlock,
    /// Ошибка декодирования записи.
    Decode(DecodeError),
}

/// Отображение виртуального адреса leaf-записью дерева таблиц.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct Translation {
    /// Физический адрес с учётом смещения внутри страницы или блока.
    pub pa: PhysicalAddress,
    /// Атрибуты leaf-записи страницы L3 либо блока L1 или L2.
    pub flags: Aarch64MemFlags,
}

impl Translation {
    fn of_leaf<L: Level>(raw: u64, va: VirtualAddress) -> Self {
        let span_mask = (1usize << L::SHIFT) - 1;
        let base = (raw & OUTPUT_ADDRESS_MASK) as usize & !span_mask;
        Self {
            pa: PhysicalAddress::new(base | (va.as_usize() & span_mask)),
            flags: Aarch64MemFlags::from_bits(raw & !(OUTPUT_ADDRESS_MASK | DESC_TYPE_MASK)),
        }
    }
}

#[derive(Clone, Copy)]
enum Leaf {
    BlockL1(Entry<L1, Block>),
    BlockL2(Entry<L2, Block>),
    Page {
        table: *mut PageTable<L3>,
        idx: usize,
    },
}

impl<A: TableAlloc> PageMapper<A> {
    /// Проходит таблицы L0->L1->L2->L3 и возвращает указатель на L3-таблицу
    /// и индекс внутри неё для leaf-записи (Page) `virt`.
    ///
    /// На L0/L1/L2 ожидается Table-дескриптор. Block (L1=1G, L2=2M) -> `HitBlock`.
    /// На L3 ожидается Page. Иначе -> `NotMapped`.
    pub fn walk_to_l3_leaf(
        &self,
        virt: PageAlignedVirtualAddress,
    ) -> Result<(*mut PageTable<L3>, usize), WalkError> {
        match self.find_leaf(virt.as_virtual())? {
            Leaf::Page { table, idx } => Ok((table, idx)),
            Leaf::BlockL1(_) | Leaf::BlockL2(_) => Err(WalkError::HitBlock),
        }
    }

    /// Находит leaf-запись для `virt` и возвращает физический адрес с её атрибутами.
    /// Функция поддерживает страницы L3 и блоки L1/L2 и возвращает `NotMapped`, если
    /// отображение отсутствует.
    pub fn translate(&self, virt: VirtualAddress) -> Result<Translation, WalkError> {
        Ok(Self::translation_of(self.find_leaf(virt)?, virt))
    }

    pub(crate) fn visit_leaves(
        &self,
        base: VirtualAddress,
        visitor: &mut impl FnMut(PageAlignedVirtualAddress, u8, u64),
    ) {
        // SAFETY: root и дочерние таблицы действительны по контракту `Self::new`;
        // заимствование `self` на чтение исключает их изменение через mapper.
        unsafe {
            L0::visit_subtree(
                self.root,
                &self.alloc,
                base.as_usize() & HIGH_HALF_MASK,
                visitor,
            );
        };
    }

    fn translation_of(leaf: Leaf, virt: VirtualAddress) -> Translation {
        match leaf {
            Leaf::Page { table, idx } => {
                // SAFETY: `find_leaf` вернул валидную таблицу L3 и индекс её Page-записи.
                let raw = unsafe { (*table).get_raw(idx) };
                Translation::of_leaf::<L3>(raw, virt)
            }
            Leaf::BlockL1(entry) => Translation::of_leaf::<L1>(entry.raw(), virt),
            Leaf::BlockL2(entry) => Translation::of_leaf::<L2>(entry.raw(), virt),
        }
    }

    fn find_leaf(&self, virt: VirtualAddress) -> Result<Leaf, WalkError> {
        let target_va = virt.as_usize();

        // SAFETY: root инициализирован в `Self::new`.
        let raw_l0 = unsafe { (*self.root).get_raw(virt.index::<L0>()) };
        let l1 = match decode::<L0>(raw_l0).map_err(WalkError::Decode)? {
            AnyEntry::Table(te) => {
                // SAFETY: `te` извлечён из валидного Table-дескриптора уровня L0,
                // `alloc.table_ptr` транслирует PA->VA согласно своему контракту.
                unsafe {
                    self.alloc
                        .table_ptr::<L1>(extract_table_pa(te.raw()), target_va)
                }
            }
            // L0 не поддерживает Block, а декодер не возвращает Page.
            AnyEntry::Invalid(_) | AnyEntry::Block(_) | AnyEntry::Page(_) => {
                return Err(WalkError::NotMapped);
            }
        };

        // SAFETY: l1 получен через alloc.table_ptr из валидного Table-дескриптора.
        let raw_l1 = unsafe { (*l1).get_raw(virt.index::<L1>()) };
        let l2 = match decode::<L1>(raw_l1).map_err(WalkError::Decode)? {
            AnyEntry::Table(te) => {
                // SAFETY: см. выше.
                unsafe {
                    self.alloc
                        .table_ptr::<L2>(extract_table_pa(te.raw()), target_va)
                }
            }
            AnyEntry::Block(block) => {
                return Ok(Leaf::BlockL1(block));
            }
            AnyEntry::Invalid(_) | AnyEntry::Page(_) => {
                return Err(WalkError::NotMapped);
            }
        };

        // SAFETY: l2 получен через alloc.table_ptr из валидного Table-дескриптора.
        let raw_l2 = unsafe { (*l2).get_raw(virt.index::<L2>()) };
        let table = match decode::<L2>(raw_l2).map_err(WalkError::Decode)? {
            AnyEntry::Table(te) => {
                // SAFETY: см. выше.
                unsafe {
                    self.alloc
                        .table_ptr::<L3>(extract_table_pa(te.raw()), target_va)
                }
            }
            AnyEntry::Block(block) => {
                return Ok(Leaf::BlockL2(block));
            }
            AnyEntry::Invalid(_) | AnyEntry::Page(_) => {
                return Err(WalkError::NotMapped);
            }
        };

        let idx = virt.index::<L3>();
        // SAFETY: table получен через alloc.table_ptr из валидного Table-дескриптора.
        let raw_l3 = unsafe { (*table).get_raw(idx) };
        // L3 содержит только Page (desc-type 0b11) либо Invalid (0b00).
        if raw_l3 & DESC_TYPE_MASK == 0b11 {
            Ok(Leaf::Page { table, idx })
        } else {
            Err(WalkError::NotMapped)
        }
    }
}

const LEAF_PA_AND_DESC_MASK: u64 = 0x0000_FFFF_FFFF_F803;

/// Меняет только биты атрибутов в L3-leaf-записи (`table[idx]`), сохраняя PA и desc-type.
///
/// Запись 8-байтового выровненного значения single-copy atomic. Race с MMU-walker
/// безопасен. Метод корректен только для **расширения** прав (RX->RW, RO->RW). Сужение
/// требует break-before-make и здесь не реализовано.
///
/// # Safety
///
/// - `table` валиден и эксклюзивно доступен (вызывающий держит lock на mapper);
/// - `idx < 512`;
/// - запись по `idx` имеет desc-type Page (0b11). Иначе вернётся `WalkError::NotMapped`
///   и таблица не изменяется.
pub unsafe fn update_l3_flags(
    table: *mut PageTable<L3>,
    idx: usize,
    new_flags: Aarch64MemFlags,
) -> Result<(), WalkError> {
    // SAFETY: см. контракт.
    let raw = unsafe { (*table).get_raw(idx) };
    if raw & DESC_TYPE_MASK != 0b11 {
        return Err(WalkError::NotMapped);
    }
    // Раскладка page-дескриптора (ARMv8-A, 4К granule): `[1:0]` desc-type (у Page
    // всегда `0b11`), `[4:2]` `AttrIndx`, `[5]` `NS`, `[7:6]` `AP`, `[9:8]` `SH`,
    // `[10]` `AF`, `[11]` `nG`, `[47:12]` PA, `[53]` `PXN`, `[54]` `UXN`.
    // Маска удерживает PA, desc-type и `nG`: бит `nG` принадлежит владеющему AS и
    // остаётся неизменным при `remap`. Остальные биты берутся из `new_flags`.
    let preserved = raw & LEAF_PA_AND_DESC_MASK;
    let new_raw = preserved | (new_flags.bits() & !LEAF_PA_AND_DESC_MASK);
    let entry = Entry::<L3, Page>::from_raw_unchecked(new_raw);
    // SAFETY: см. контракт.
    unsafe { (*table).set(idx, entry) };
    Ok(())
}

/// Колбэки обхода таблиц. Порядок вызовов определяется операцией обхода.
pub trait TableVisitor {
    /// Leaf-запись по `va` уровня `leaf_shift` (`L3::SHIFT`/`L2::SHIFT`/`L1::SHIFT`).
    /// `raw` - значение PTE.
    /// В `unmap_range` вызов предшествует обнулению записи, а записи, снятые раньше, уже
    /// обнулены. `free_all` записей не обнуляет.
    fn on_leaf(&mut self, va: PageAlignedVirtualAddress, leaf_shift: u8, raw: u64);
    /// Дочерняя таблица после обхода её содержимого. В `unmap_range` таблица пуста,
    /// а ссылающаяся на неё запись родителя уже обнулена.
    fn on_table(&mut self, pa: PageAlignedAddress);
}

impl<F: FnMut(PageAlignedVirtualAddress, u8, u64)> TableVisitor for F {
    fn on_leaf(&mut self, va: PageAlignedVirtualAddress, leaf_shift: u8, raw: u64) {
        self(va, leaf_shift, raw);
    }

    fn on_table(&mut self, _pa: PageAlignedAddress) {}
}

/// Ошибки `unmap_range`.
#[derive(Debug, PartialEq, Eq)]
pub enum UnmapError {
    /// Invalid-запись внутри запрошенного диапазона.
    NotMapped,
    /// Диапазон частично пересекает block-leaf. Split не поддерживается.
    UnsupportedPartialBlock,
    /// Невалидный дескриптор.
    Decode(DecodeError),
}

const HIGH_HALF_MASK: usize = 0xFFFF_0000_0000_0000;

/// Снимает leaf-записи в `[va, va + size)`. Опустевшие L1/L2/L3
/// возвращаются через `visitor.on_table`. Block-leaf снимается
/// только целиком. Partial -> [`UnmapError::UnsupportedPartialBlock`].
///
/// Отказ на середине диапазона не отменяет уже обработанные leaf-записи этого вызова. Они
/// остаются снятыми. `visitor` уже получил по ним вызовы `on_leaf`.
///
/// TLB не инвалидируется. Вызывающий инвалидирует его после снятия записей. До инвалидации
/// трансляции по снятым записям могут сохраняться в TLB тех CPU, где адресное пространство
/// активно.
///
/// # Safety
/// `root` валиден, `alloc.table_ptr` транслирует PA->VA для всех subtree,
/// caller держит эксклюзивный доступ к таблицам.
pub unsafe fn unmap_range<A, V>(
    root: *mut PageTable<L0>,
    alloc: &A,
    va: usize,
    size: usize,
    visitor: &mut V,
) -> Result<(), UnmapError>
where
    A: TableAlloc,
    V: TableVisitor + ?Sized,
{
    if size == 0 {
        return Ok(());
    }
    let canonical_top = va & HIGH_HALF_MASK;
    let range_last = va + (size - 1);
    // SAFETY: см. контракт.
    let _ = unsafe { L0::unmap_subtree(root, alloc, canonical_top, va, range_last, visitor)? };
    Ok(())
}

/// Обходит дерево таблиц перед уничтожением адресного пространства.
/// Обход вызывает `visitor.on_leaf` для каждого листа и `visitor.on_table` для каждой
/// дочерней таблицы L1/L2/L3 после обхода её содержимого. Вызывающий код отдельно решает,
/// нужно ли освобождать корень. Обход пропускает пустые и повреждённые дескрипторы.
///
/// # Safety
/// см. [`unmap_range`]. AS не активен ни на одном CPU.
pub unsafe fn free_all<A, V>(root: *mut PageTable<L0>, alloc: &A, high_half: bool, visitor: &mut V)
where
    A: TableAlloc,
    V: TableVisitor + ?Sized,
{
    let canonical_top = if high_half { HIGH_HALF_MASK } else { 0 };
    // SAFETY: см. контракт.
    unsafe { L0::visit_subtree(root, alloc, canonical_top, visitor) };
}

trait TableWalk: Level + Sized {
    // SAFETY: см. [`unmap_range`].
    unsafe fn unmap_subtree<A, V>(
        table: *mut PageTable<Self>,
        alloc: &A,
        table_va_base: usize,
        range_start: usize,
        range_last: usize,
        visitor: &mut V,
    ) -> Result<bool, UnmapError>
    where
        A: TableAlloc,
        V: TableVisitor + ?Sized;

    // SAFETY: таблицы действительны и защищены от конкурентного изменения. Посетитель
    // может освободить leaf в `on_leaf` и дочернюю таблицу в `on_table`, если AS не активен.
    unsafe fn visit_subtree<A, V>(
        table: *mut PageTable<Self>,
        alloc: &A,
        table_va_base: usize,
        visitor: &mut V,
    ) where
        A: TableAlloc,
        V: TableVisitor + ?Sized;
}

macro_rules! impl_table_walk {
    ($level:ty, $child:ty) => {
        impl TableWalk for $level {
            unsafe fn unmap_subtree<A, V>(
                table: *mut PageTable<$level>,
                alloc: &A,
                table_va_base: usize,
                range_start: usize,
                range_last: usize,
                visitor: &mut V,
            ) -> Result<bool, UnmapError>
            where
                A: TableAlloc,
                V: TableVisitor + ?Sized,
            {
                // SAFETY: контракт прокидывается из caller.
                unsafe {
                    descend_unmap::<$level, $child, A, V>(
                        table,
                        alloc,
                        table_va_base,
                        range_start,
                        range_last,
                        visitor,
                    )
                }
            }

            unsafe fn visit_subtree<A, V>(
                table: *mut PageTable<$level>,
                alloc: &A,
                table_va_base: usize,
                visitor: &mut V,
            ) where
                A: TableAlloc,
                V: TableVisitor + ?Sized,
            {
                // SAFETY: см. caller.
                unsafe {
                    descend_visit::<$level, $child, A, V>(table, alloc, table_va_base, visitor)
                }
            }
        }
    };
}

impl_table_walk!(L0, L1);
impl_table_walk!(L1, L2);
impl_table_walk!(L2, L3);

impl TableWalk for L3 {
    unsafe fn unmap_subtree<A, V>(
        table: *mut PageTable<L3>,
        _alloc: &A,
        table_va_base: usize,
        range_start: usize,
        range_last: usize,
        visitor: &mut V,
    ) -> Result<bool, UnmapError>
    where
        A: TableAlloc,
        V: TableVisitor + ?Sized,
    {
        let (first, last) = idx_range::<L3>(range_start, range_last);
        for idx in first..last {
            let entry_va = leaf_va(entry_va_at::<L3>(table_va_base, idx));
            // SAFETY: idx in [0, 512), table валиден (caller-lock).
            let raw = unsafe { (*table).get_raw(idx) };
            // L3: только Page (0b11) или Invalid (0b00); Block невозможен.
            match raw & DESC_TYPE_MASK {
                0b00 => return Err(UnmapError::NotMapped),
                0b11 => {
                    visitor.on_leaf(entry_va, L3::SHIFT, raw);
                    // SAFETY: caller-lock; idx валиден.
                    unsafe { (*table).set(idx, Entry::<L3, Invalid>::invalid()) };
                }
                _ => return Err(UnmapError::Decode(DecodeError::Reserved)),
            }
        }
        // SAFETY: caller-lock.
        Ok(unsafe { table_is_empty::<L3>(table) })
    }

    unsafe fn visit_subtree<A, V>(
        table: *mut PageTable<L3>,
        _alloc: &A,
        table_va_base: usize,
        visitor: &mut V,
    ) where
        A: TableAlloc,
        V: TableVisitor + ?Sized,
    {
        for idx in 0..512 {
            // SAFETY: caller-lock.
            let raw = unsafe { (*table).get_raw(idx) };
            if raw & DESC_TYPE_MASK == 0b11 {
                visitor.on_leaf(
                    leaf_va(entry_va_at::<L3>(table_va_base, idx)),
                    L3::SHIFT,
                    raw,
                );
            }
        }
    }
}

// SAFETY: см. [`unmap_range`].
unsafe fn descend_unmap<Parent, Child, A, V>(
    table: *mut PageTable<Parent>,
    alloc: &A,
    table_va_base: usize,
    range_start: usize,
    range_last: usize,
    visitor: &mut V,
) -> Result<bool, UnmapError>
where
    Parent: Level + CanTable + DecodeBlock,
    Child: TableWalk,
    A: TableAlloc,
    V: TableVisitor + ?Sized,
{
    let entry_size = 1usize << Parent::SHIFT;
    let (first, last) = idx_range::<Parent>(range_start, range_last);
    for idx in first..last {
        let entry_va = entry_va_at::<Parent>(table_va_base, idx);
        let entry_last = entry_va + (entry_size - 1);
        // SAFETY: idx in [0, 512), table валиден (caller-lock).
        let raw = unsafe { (*table).get_raw(idx) };
        match decode::<Parent>(raw).map_err(UnmapError::Decode)? {
            AnyEntry::Invalid(_) => return Err(UnmapError::NotMapped),
            AnyEntry::Block(_) => {
                if entry_va < range_start || entry_last > range_last {
                    return Err(UnmapError::UnsupportedPartialBlock);
                }
                visitor.on_leaf(leaf_va(entry_va), Parent::SHIFT, raw);
                // SAFETY: caller-lock.
                unsafe { (*table).set(idx, Entry::<Parent, Invalid>::invalid()) };
            }
            AnyEntry::Table(te) => {
                let child_pa = extract_table_pa(te.raw());
                // SAFETY: child_pa из валидного Table-дескриптора Parent.
                let child = unsafe { alloc.table_ptr::<Child>(child_pa, entry_va) };
                let sub_start = range_start.max(entry_va);
                let sub_last = range_last.min(entry_last);
                // SAFETY: child получен через alloc.table_ptr; caller-lock.
                let now_empty = unsafe {
                    Child::unmap_subtree(child, alloc, entry_va, sub_start, sub_last, visitor)?
                };
                if now_empty {
                    // SAFETY: caller-lock.
                    unsafe { (*table).set(idx, Entry::<Parent, Invalid>::invalid()) };
                    visitor.on_table(child_pa);
                }
            }
            // L0/L1/L2 не имеют Page; corrupt дескриптор - сообщаем Decode.
            AnyEntry::Page(_) => return Err(UnmapError::Decode(DecodeError::WrongKind)),
        }
    }
    // SAFETY: caller-lock.
    Ok(unsafe { table_is_empty::<Parent>(table) })
}

// SAFETY: см. `TableWalk::visit_subtree`.
unsafe fn descend_visit<Parent, Child, A, V>(
    table: *mut PageTable<Parent>,
    alloc: &A,
    table_va_base: usize,
    visitor: &mut V,
) where
    Parent: Level + CanTable + DecodeBlock,
    Child: TableWalk,
    A: TableAlloc,
    V: TableVisitor + ?Sized,
{
    for idx in 0..512 {
        let entry_va = entry_va_at::<Parent>(table_va_base, idx);
        // SAFETY: caller-lock.
        let raw = unsafe { (*table).get_raw(idx) };
        let Ok(entry) = decode::<Parent>(raw) else {
            continue;
        };
        match entry {
            AnyEntry::Invalid(_) | AnyEntry::Page(_) => (),
            AnyEntry::Block(_) => visitor.on_leaf(leaf_va(entry_va), Parent::SHIFT, raw),
            AnyEntry::Table(te) => {
                let child_pa = extract_table_pa(te.raw());
                // SAFETY: child_pa из валидного Table-дескриптора Parent.
                let child = unsafe { alloc.table_ptr::<Child>(child_pa, entry_va) };
                // SAFETY: см. caller.
                unsafe { Child::visit_subtree(child, alloc, entry_va, visitor) };
                visitor.on_table(child_pa);
            }
        }
    }
}

#[inline]
fn leaf_va(va: usize) -> PageAlignedVirtualAddress {
    debug_assert!(
        va.is_multiple_of(PageAlignedVirtualAddress::ALIGNMENT),
        "leaf VA must be 4K-aligned"
    );
    PageAlignedVirtualAddress::aligned_down(VirtualAddress::new(va))
}

#[inline]
fn idx_at<L: Level>(va: usize) -> usize {
    (va >> L::SHIFT) & 0x1FF
}

#[inline]
fn idx_range<L: Level>(range_start: usize, range_last: usize) -> (usize, usize) {
    let first = idx_at::<L>(range_start);
    let last_inc = idx_at::<L>(range_last);
    (first, last_inc + 1)
}

#[inline]
fn entry_va_at<L: Level>(table_va_base: usize, idx: usize) -> usize {
    table_va_base | (idx << L::SHIFT)
}

// SAFETY: `table` валиден; caller владеет эксклюзивным доступом.
#[allow(clippy::verbose_bit_mask)]
unsafe fn table_is_empty<L: Level>(table: *mut PageTable<L>) -> bool {
    (0..512).all(|idx| {
        // SAFETY: см. контракт.
        unsafe { (*table).get_raw(idx) & 0b11 == 0 }
    })
}
