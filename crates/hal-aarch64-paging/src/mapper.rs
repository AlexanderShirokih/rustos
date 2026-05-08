//! Маппинг виртуальных адресов на физические.

use memory::{
    physical_address::{PageAlignedAddress, PhysicalAddress},
    virtual_address::{AlignedVirtualAddress, PageAlignedVirtualAddress},
};

use crate::{
    entry::{AnyEntry, Block, CanTable, DecodeBlock, DecodeError, Entry, Page, Table, decode},
    level::{L0, L1, L1BlockPa, L2, L2BlockPa, L3, Level, PagePa},
    mem_flags::Aarch64MemFlags,
    page_table::PageTable,
    table_alloc::TableAlloc,
    table_flags::TableFlags,
    virtual_address::VirtualAddressExt,
};

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
    /// Корневая таблица L0.
    root: *mut PageTable<L0>,
    /// Аллокатор таблиц страниц.
    alloc: A,
    /// Флаги для новых записей-таблиц.
    table_flags: TableFlags,
}

impl<A: TableAlloc> PageMapper<A> {
    /// Создаёт маппер с заданной корневой таблицей и аллокатором.
    ///
    /// Table-флаги (PXNTable/UXNTable/APTable) оставляем нулевыми: эти биты
    /// - иерархическое **ужесточение** (могут только запрещать), и если
    /// поставить, например, UXNTable=1, то любая leaf-страница ниже становится
    /// non-executable для EL0 даже при UXN=0 в leaf-дескрипторе. Это ломает
    /// userspace-маппинги, у которых leaf явно UXN=0 (UserRX). Защита kernel-only
    /// страниц обеспечивается leaf-флагами (`Heap::flags`/`KernelData::flags`/
    /// `KernelRoData::flags` и пр. уже выставляют `uxn(true)`/`pxn(true)`).
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

    /// Создаёт маппинг виртуального адреса на физический.
    pub fn map_page<const SHIFT: u8, P: MapLeaf<SHIFT>>(
        &mut self,
        virt: AlignedVirtualAddress<SHIFT>,
        phys: P,
        flags: Aarch64MemFlags,
    ) -> Result<(), MapError> {
        P::map_into(self, virt, phys, flags)
    }

    /// Гарантирует наличие дочерней таблицы в `parent` для `target_va`.
    ///
    /// Если записи нет - выделяет новую таблицу.
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

                // SAFETY: parent валиден (см. ensure_next caller), idx в пределах 0..512;
                // запись Table-entry создаёт маппинг для свежевыделенной страницы.
                unsafe { (*parent).set(idx, Entry::<PL, Table>::new(child_pa, self.table_flags)) };

                // SAFETY: child_pa только что выделен alloc-ом и связан с parent[idx],
                // alloc.table_ptr выдаёт корректный VA для последующей инициализации.
                let child = unsafe { self.alloc.table_ptr::<CL>(child_pa, target_va) };
                // SAFETY: child указывает на свежевыделенную страницу, эксклюзивно владеемую mapper-ом
                // до конца этой функции; запись пустой PageTable инициализирует все 512 entries в Invalid.
                unsafe { child.write(PageTable::new()) };

                Ok(child)
            }

            AnyEntry::Block(_) | AnyEntry::Page(_) => Err(MapError::AlreadyMapped),
        }
    }
}

/// Извлекает физический адрес таблицы из дескриптора.
pub fn extract_table_pa(raw: u64) -> PageAlignedAddress {
    PageAlignedAddress::new_unchecked(PhysicalAddress::new((raw & 0x0000_FFFF_FFFF_F000) as usize))
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

impl<A: TableAlloc> PageMapper<A> {
    /// Проходит таблицы L0->L1->L2->L3 и возвращает указатель на L3-таблицу
    /// и индекс внутри неё для leaf-записи (Page) `virt`.
    ///
    /// На L0/L1/L2 ожидается Table-дескриптор; Block (L1=1G, L2=2M) -> `HitBlock`.
    /// На L3 ожидается Page; иначе -> `NotMapped`.
    pub fn walk_to_l3_leaf(
        &self,
        virt: PageAlignedVirtualAddress,
    ) -> Result<(*mut PageTable<L3>, usize), WalkError> {
        let target_va = virt.as_usize();

        let l0 = self.root;
        // SAFETY: root инициализирован в `Self::new`.
        let raw_l0 = unsafe { (*l0).get_raw(virt.index::<L0>()) };
        let l1 = match decode::<L0>(raw_l0).map_err(WalkError::Decode)? {
            AnyEntry::Invalid(_) => return Err(WalkError::NotMapped),
            AnyEntry::Table(te) => {
                let pa = extract_table_pa(te.raw());
                // SAFETY: `te` извлечён из валидного Table-дескриптора уровня L0,
                // `alloc.table_ptr` транслирует PA->VA согласно своему контракту.
                unsafe { self.alloc.table_ptr::<L1>(pa, target_va) }
            }
            // L0 не поддерживает Block; Page тут возможен только при сломанном
            // дескрипторе. В любом случае - это не Table, идти ниже некуда.
            AnyEntry::Block(_) | AnyEntry::Page(_) => return Err(WalkError::HitBlock),
        };

        // SAFETY: l1 получен через alloc.table_ptr из валидного Table-дескриптора.
        let raw_l1 = unsafe { (*l1).get_raw(virt.index::<L1>()) };
        let l2 = match decode::<L1>(raw_l1).map_err(WalkError::Decode)? {
            // Invalid - pte отсутствует; Page (на L1 не должен встречаться, но
            // если встретился - корявый дескриптор) - нечего ремаппить.
            AnyEntry::Invalid(_) | AnyEntry::Page(_) => return Err(WalkError::NotMapped),
            AnyEntry::Table(te) => {
                let pa = extract_table_pa(te.raw());
                // SAFETY: см. выше.
                unsafe { self.alloc.table_ptr::<L2>(pa, target_va) }
            }
            AnyEntry::Block(_) => return Err(WalkError::HitBlock),
        };

        // SAFETY: l2 получен через alloc.table_ptr из валидного Table-дескриптора.
        let raw_l2 = unsafe { (*l2).get_raw(virt.index::<L2>()) };
        let l3 = match decode::<L2>(raw_l2).map_err(WalkError::Decode)? {
            // См. комментарий выше для L1.
            AnyEntry::Invalid(_) | AnyEntry::Page(_) => return Err(WalkError::NotMapped),
            AnyEntry::Table(te) => {
                let pa = extract_table_pa(te.raw());
                // SAFETY: см. выше.
                unsafe { self.alloc.table_ptr::<L3>(pa, target_va) }
            }
            AnyEntry::Block(_) => return Err(WalkError::HitBlock),
        };

        let idx = virt.index::<L3>();
        // SAFETY: l3 получен через alloc.table_ptr из валидного Table-дескриптора.
        let raw_l3 = unsafe { (*l3).get_raw(idx) };
        // L3 содержит только Page (desc-type 0b11) либо Invalid (0b00).
        if raw_l3 & 0b11 == 0b11 {
            Ok((l3, idx))
        } else {
            Err(WalkError::NotMapped)
        }
    }
}

/// Маска битов, которые сохраняются при обновлении флагов leaf-записи `Entry<L3, Page>`:
/// PA (биты `[47:12]`) и desc-type (биты `[1:0]`, для Page всегда `0b11`).
///
/// Битовая раскладка page-дескриптора (ARMv8-A, 4К granule):
/// - `[1:0]`   - desc-type;
/// - `[4:2]`   - `AttrIndx` (индекс MAIR);
/// - `[5]`     - `NS` (non-secure);
/// - `[7:6]`   - `AP` (права доступа EL0/EL1);
/// - `[9:8]`   - `SH` (shareability);
/// - `[10]`    - `AF` (access flag);
/// - `[11]`    - `nG` (not-global);
/// - `[47:12]` - physical-address биты;
/// - `[53]`    - `PXN`;
/// - `[54]`    - `UXN`.
///
/// `update_l3_flags` сохраняет PA, desc-type и бит `nG` (свойство владельца
/// AS, а не permission'ов - не должно меняться при `remap`); остальное
/// (`AttrIndx`, `AP`, `SH`, `AF`, `PXN`, `UXN`, …) перезаписывается из
/// `new_flags`. `0xF803` - это `0b1111_1000_0000_0011`: биты `[1:0]`
/// (desc-type), `[11]` (nG) и `[12:15]` как часть PA-mask
/// `0x0000_FFFF_FFFF_F000`.
const LEAF_PA_AND_DESC_MASK: u64 = 0x0000_FFFF_FFFF_F803;

/// Меняет только биты атрибутов в L3-leaf-записи (`table[idx]`), сохраняя PA и desc-type.
///
/// Запись 8-байтового выровненного значения single-copy atomic; race с MMU-walker
/// безопасен. Метод корректен только для **расширения** прав (RX->RW, RO->RW); сужение
/// требует break-before-make и здесь не реализовано.
///
/// # Safety
///
/// - `table` валиден и эксклюзивно доступен (вызывающий держит lock на mapper);
/// - `idx < 512`;
/// - запись по `idx` имеет desc-type Page (0b11); иначе вернётся `WalkError::NotMapped`
///   и таблица не изменяется.
pub unsafe fn update_l3_flags(
    table: *mut PageTable<L3>,
    idx: usize,
    new_flags: Aarch64MemFlags,
) -> Result<(), WalkError> {
    // SAFETY: см. контракт.
    let raw = unsafe { (*table).get_raw(idx) };
    if raw & 0b11 != 0b11 {
        return Err(WalkError::NotMapped);
    }
    let preserved = raw & LEAF_PA_AND_DESC_MASK;
    let new_raw = preserved | (new_flags.bits() & !LEAF_PA_AND_DESC_MASK);
    let entry = Entry::<L3, Page>::from_raw_unchecked(new_raw);
    // SAFETY: см. контракт.
    unsafe { (*table).set(idx, entry) };
    Ok(())
}
