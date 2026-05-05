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
    pub fn new(root: *mut PageTable<L0>, alloc: A) -> Self {
        Self {
            root,
            alloc,
            table_flags: TableFlags::new().pxn_table(false).uxn_table(true),
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
                let child = unsafe { self.alloc.table_ptr::<CL>(child_pa, target_va) };
                Ok(child)
            }

            AnyEntry::Invalid(_) => {
                let child_pa = self.alloc.alloc_table_page().ok_or(MapError::OutOfMemory)?;

                // Запись entry в parent - создание маппинга через recursive
                unsafe { (*parent).set(idx, Entry::<PL, Table>::new(child_pa, self.table_flags)) };

                // Теперь получаем указатель и инициализируем таблицу
                let child = unsafe { self.alloc.table_ptr::<CL>(child_pa, target_va) };
                unsafe { child.write(PageTable::new()) };

                Ok(child)
            }

            AnyEntry::Block(_) | AnyEntry::Page(_) => Err(MapError::AlreadyMapped),
        }
    }
}

/// Извлекает физический адрес таблицы из дескриптора.
fn extract_table_pa(raw: u64) -> PageAlignedAddress {
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
