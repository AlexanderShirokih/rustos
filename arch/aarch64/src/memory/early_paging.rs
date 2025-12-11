use crate::memory::mmu::MmuConfig;
use aarch64_paging::{EntryFlags, PageSize, PageTable, PageTableEntry};
use core::cell::UnsafeCell;
use core::mem::MaybeUninit;
use memory::memory_backend::MemoryBackend;
use memory::physical::PhysicalAddress;

/// Конфигурация раннего MMU (таблицы уже подготовлены).
pub struct EarlyMMUConfig {
    root_table: PhysicalAddress,
}

impl EarlyMMUConfig {
    /// Создать конфигурацию раннего identity mapping.
    /// identity-map первых 4GB 1GB блоками (L1).
    pub fn create<B: MemoryBackend>(backend: &B) -> Self {
        let l0_phys = early_table_phys(&EARLY_L0);
        let l1_phys = early_table_phys(&EARLY_L1);

        // Собираем таблицы на стеке и затем записываем их через MemoryBackend.
        // Это позволяет early paging работать не только с прямыми указателями.
        let mut l0 = PageTable::empty();
        let mut l1 = PageTable::empty();

        // L1: 4 записи по 1GB: 0..4GB
        for i in 0..4usize {
            let phys = i * 0x4000_0000usize;
            l1[i] = PageTableEntry::new_leaf(
                PhysicalAddress::from(phys),
                PageSize::Size1G,
                EntryFlags::KERNEL_DATA,
            );
        }

        // L0[0] -> L1
        let l1_ptr = l1_phys.as_usize() as *const PageTable;
        l0[0] = PageTableEntry::new_table_from_ptr(l1_ptr);

        // Сначала пишем L1, затем L0 (который на него ссылается).
        backend.write::<PageTable>(l1_phys, l1);
        backend.write::<PageTable>(l0_phys, l0);

        // Убедимся, что таблицы страниц видимы до включения MMU.
        backend.clean_page_cache(l0_phys);
        backend.clean_page_cache(l1_phys);
        backend.invalidate_cache();

        Self {
            root_table: l0_phys,
        }
    }
}

impl MmuConfig for EarlyMMUConfig {
    fn root_table(&self) -> PhysicalAddress {
        self.root_table
    }
}

struct EarlyTableCell(UnsafeCell<MaybeUninit<PageTable>>);
// SAFETY: используется только на раннем этапе инициализации (однопоточно).
unsafe impl Sync for EarlyTableCell {}

fn early_table_phys(cell: &EarlyTableCell) -> PhysicalAddress {
    // MaybeUninit<PageTable> имеет тот же layout и alignment, что и PageTable.
    PhysicalAddress::from(cell.0.get() as usize)
}

static EARLY_L0: EarlyTableCell = EarlyTableCell(UnsafeCell::new(MaybeUninit::uninit()));
static EARLY_L1: EarlyTableCell = EarlyTableCell(UnsafeCell::new(MaybeUninit::uninit()));
