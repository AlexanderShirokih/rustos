use crate::entry::{AnyEntry, CanTable, DecodeError, Entry, Page, Table, decode};
use crate::level::{L0, L1, L2, L3, Level};
use crate::mem_flags::MemFlags;
use crate::page_table::PageTable;
use crate::table_alloc::TableAlloc;
use crate::table_flags::TableFlags;
use crate::virtual_address::VirtualAddressExt;
use memory::physical_address::{PageAlignedAddress, PhysicalAddress};
use memory::virtual_address::{AlignedVirtualAddress, PageAlignedVirtualAddress};

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

    pub fn map_page(
        &mut self,
        virt: PageAlignedVirtualAddress,
        phys: PageAlignedAddress,
        flags: MemFlags,
    ) -> Result<(), MapError> {
        let l0 = self.root;
        let l1 = self.ensure_next::<L0, L1, _>(l0, virt)?;
        let l2 = self.ensure_next::<L1, L2, _>(l1, virt)?;
        let l3 = self.ensure_next::<L2, L3, _>(l2, virt)?;

        let idx = virt.index::<L3>();

        // SAFETY: l3 получен через ensure_next, который гарантирует валидность указателя
        unsafe { (*l3).set(idx, Entry::<L3, Page>::new(phys, flags)) };

        Ok(())
    }

    /// Убедиться, что в таблице `parent` по индексу для `virt` есть ссылка на дочернюю таблицу.
    /// Если записи нет — выделить новую таблицу.
    /// Возвращает указатель на дочернюю таблицу уровня `CL`.
    fn ensure_next<PL, CL, const SHIFT: u8>(
        &mut self,
        parent: *mut PageTable<PL>,
        virt: AlignedVirtualAddress<SHIFT>,
    ) -> Result<*mut PageTable<CL>, MapError>
    where
        PL: Level + CanTable,
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

            AnyEntry::Block(_) | AnyEntry::Page(_) => Err(MapError::OccupiedByLeaf),
        }
    }
}

fn extract_table_pa(raw: u64) -> PageAlignedAddress {
    PageAlignedAddress::new_unchecked(PhysicalAddress::new((raw & 0x0000_FFFF_FFFF_F000) as usize))
}

#[derive(Debug)]
pub enum MapError {
    OutOfMemory,
    OccupiedByLeaf, // вместо Table уже стоит Block/Page
    BadAlignment,
    Decode(DecodeError),
}
