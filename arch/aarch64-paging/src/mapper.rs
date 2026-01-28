use crate::entry::{
    AnyEntry, Block, CanTable, DecodeBlock, DecodeError, Entry, Page, Table, decode,
};
use crate::level::{L0, L1, L1BlockPa, L2, L2BlockPa, L3, Level, PagePa};
use crate::mem_flags::MemFlags;
use crate::page_table::PageTable;
use crate::table_alloc::TableAlloc;
use crate::table_flags::TableFlags;
use crate::virtual_address::VirtualAddressExt;
use memory::physical_address::{PageAlignedAddress, PhysicalAddress};
use memory::virtual_address::{AlignedVirtualAddress, PageAlignedVirtualAddress};

pub trait MapLeaf<const SHIFT: u8>: Copy {
    fn map_into<A: TableAlloc>(
        mapper: &mut PageMapper<A>,
        virt: AlignedVirtualAddress<SHIFT>,
        phys: Self,
        flags: MemFlags,
    ) -> Result<(), MapError>;
}

impl MapLeaf<{ L3::SHIFT }> for PagePa {
    fn map_into<A: TableAlloc>(
        mapper: &mut PageMapper<A>,
        virt: PageAlignedVirtualAddress,
        phys: Self,
        flags: MemFlags,
    ) -> Result<(), MapError> {
        let l0 = mapper.root;
        let l1 = mapper.ensure_next::<L0, L1, { L3::SHIFT }>(l0, virt)?;
        let l2 = mapper.ensure_next::<L1, L2, { L3::SHIFT }>(l1, virt)?;
        let l3 = mapper.ensure_next::<L2, L3, { L3::SHIFT }>(l2, virt)?;
        let idx = virt.index::<L3>();

        // SAFETY: l3 получен через ensure_next, который гарантирует валидность указателя
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
        flags: MemFlags,
    ) -> Result<(), MapError> {
        let l0 = mapper.root;
        let l1 = mapper.ensure_next::<L0, L1, { L2::SHIFT }>(l0, virt)?;
        let l2 = mapper.ensure_next::<L1, L2, { L2::SHIFT }>(l1, virt)?;
        let idx = virt.index::<L2>();

        // SAFETY: l2 получен через ensure_next, который гарантирует валидность указателя
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
        flags: MemFlags,
    ) -> Result<(), MapError> {
        let l0 = mapper.root;
        let l1 = mapper.ensure_next::<L0, L1, { L1::SHIFT }>(l0, virt)?;
        let idx = virt.index::<L1>();

        // SAFETY: l1 получен через ensure_next, который гарантирует валидность указателя
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

pub struct PageMapper<A: TableAlloc> {
    root: *mut PageTable<L0>,
    alloc: A,
    table_flags: TableFlags,
}

impl<'a, A: TableAlloc> PageMapper<A> {
    pub fn new(root: *mut PageTable<L0>, alloc: A) -> Self {
        Self {
            root,
            alloc,
            table_flags: TableFlags::new().pxn_table(false).uxn_table(true),
        }
    }

    pub fn map_page<const SHIFT: u8, P: MapLeaf<SHIFT>>(
        &mut self,
        virt: AlignedVirtualAddress<SHIFT>,
        phys: P,
        flags: MemFlags,
    ) -> Result<(), MapError> {
        P::map_into(self, virt, phys, flags)
    }

    /// Убедиться, что в таблице `parent` по индексу для `virt` есть ссылка на дочернюю таблицу.
    /// Если записи нет — выделить новую таблицу.
    /// Возвращает указатель на дочернюю таблицу уровня `CL`.
    fn ensure_next<PL, CL, const SHIFT: u8>(
        &mut self,
        parent: *mut PageTable<PL>,
        virt: AlignedVirtualAddress<{ SHIFT }>,
    ) -> Result<*mut PageTable<CL>, MapError>
    where
        PL: Level + CanTable + DecodeBlock,
        CL: Level,
    {
        let idx = virt.index::<PL>();

        // SAFETY: parent получен из self.root или предыдущего вызова ensure_next
        let raw = unsafe { (*parent).get_raw(idx) };

        match decode::<PL>(raw).map_err(MapError::Decode)? {
            AnyEntry::Table(te) => {
                let child_pa = extract_table_pa(te.raw());

                // SAFETY: child_pa указывает на существующую таблицу, созданную ранее
                let child = unsafe { self.alloc.table_ptr::<CL>(child_pa) };
                Ok(child)
            }

            AnyEntry::Invalid(_) => {
                let child_pa = self.alloc.alloc_table_page().ok_or(MapError::OutOfMemory)?;
                let child = unsafe { self.alloc.table_ptr::<CL>(child_pa) };

                unsafe { child.write(PageTable::new()) };
                unsafe { (*parent).set(idx, Entry::<PL, Table>::new(child_pa, self.table_flags)) };

                Ok(child)
            }

            AnyEntry::Block(_) | AnyEntry::Page(_) => Err(MapError::AlreadyMapped),
        }
    }
}

fn extract_table_pa(raw: u64) -> PageAlignedAddress {
    PageAlignedAddress::new_unchecked(PhysicalAddress::new((raw & 0x0000_FFFF_FFFF_F000) as usize))
}

#[derive(Debug)]
pub enum MapError {
    OutOfMemory,
    /// В ячейке целевого уровня уже стоит `Table`, поэтому нужно маппить меньшими страницами.
    NeedsSmallerPages,
    /// В ячейке уже стоит leaf (Block/Page) — конфликт маппинга.
    AlreadyMapped,
    Decode(DecodeError),
}
