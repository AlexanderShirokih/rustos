//! Платформенная проверка трансляции `MemFlags` в биты leaf-PTE user-AS.
//!
//! Единственное место, где наблюдается фактическая кодировка прав страницы:
//! host-тесты `hal-aarch64-paging` работают с `Aarch64MemFlags` уже после
//! конвертации, а из EL0 биты PTE не видны вовсе. Проверяются оба направления
//! `map_exact` -> `remap`, потому что смена флагов идёт отдельным путём
//! (`update_l3_flags`), а не пересозданием leaf'а.
#![allow(unsafe_code)]

extern crate alloc;

use kernel_tests::kernel_test;
use kernelspace::syscall_bridge;
use memory::{MemFlags, memory_mapper::MemoryMapper, virtual_address::PageAlignedVirtualAddress};
use scheduler::AddressSpace;

use crate::memory::address_space_factory::UserAarch64MemoryMapper;

const PAGE_SIZE: usize = memory::PAGE_SIZE.get();

/// Lower-half VA, не пересекающаяся с другими user-AS-тестами.
const PROBE_VA: usize = 0x5200_0000;

/// Поля leaf-дескриптора AArch64, разобранные на биты.
struct Leaf {
    raw: u64,
}

impl Leaf {
    /// `[1:0]` - тип дескриптора; `0b11` для 4K-страницы.
    fn is_page(&self) -> bool {
        self.raw & 0b11 == 0b11
    }
    /// `AP[7:6]`: `0b01` - UserRW, `0b11` - UserRO.
    fn ap(&self) -> u64 {
        (self.raw >> 6) & 0b11
    }
    /// `AF[10]`: страница помечена доступной, иначе первый доступ уйдёт в fault.
    fn af(&self) -> u64 {
        (self.raw >> 10) & 1
    }
    /// `nG[11]`: user-страницы тегируются ASID и не попадают в global-TLB.
    fn ng(&self) -> u64 {
        (self.raw >> 11) & 1
    }
    /// `PXN[53]`: исполнение с EL1 запрещено на любой user-странице.
    fn pxn(&self) -> u64 {
        (self.raw >> 53) & 1
    }
    /// `UXN[54]`: исполнение с EL0.
    fn uxn(&self) -> u64 {
        (self.raw >> 54) & 1
    }
}

/// Downcast'ит `&dyn MemoryMapper`, выданный `Aarch64AddressSpaceFactory`,
/// до конкретного типа ради `query_leaf_raw`.
fn downcast_user_mapper(mapper: &(dyn MemoryMapper + Send + Sync)) -> &UserAarch64MemoryMapper {
    mapper
        .as_any()
        .downcast_ref::<UserAarch64MemoryMapper>()
        .expect("user-AS mapper must be Aarch64MemoryMapper")
}

fn leaf(mapper: &(dyn MemoryMapper + Send + Sync), va: PageAlignedVirtualAddress) -> Leaf {
    Leaf {
        raw: downcast_user_mapper(mapper)
            .query_leaf_raw(va)
            .expect("leaf must exist"),
    }
}

/// `user_rw()` даёт AP=UserRW и запрет исполнения, `remap` в `user_rx()`
/// переводит ту же страницу в AP=UserRO с разрешённым EL0-исполнением.
/// Оба состояния - nG=1, AF=1, PXN=1: EL1 не исполняет user-память никогда.
#[kernel_test]
fn user_page_flags_map_and_remap() {
    let factory =
        syscall_bridge::address_space_factory().expect("address space factory must be installed");
    let fa = syscall_bridge::frame_allocator().expect("FrameAllocator must be installed");

    let user_as = AddressSpace::new_user(factory).expect("create user AS");
    let mapper = user_as.mapper().expect("user variant has mapper");
    let frame = fa.allocate_frame().expect("RAM frame available");
    let va = PageAlignedVirtualAddress::from_usize(PROBE_VA).expect("PROBE_VA aligned");

    mapper
        .map_exact(va, frame.page_address(), PAGE_SIZE, MemFlags::user_rw())
        .expect("map_exact probe page as UserRW");

    let rw = leaf(mapper, va);
    kernel_tests::kassert!(rw.is_page());
    kernel_tests::kassert_eq!(rw.ap(), 0b01);
    kernel_tests::kassert_eq!(rw.af(), 1);
    kernel_tests::kassert_eq!(rw.ng(), 1);
    kernel_tests::kassert_eq!(rw.pxn(), 1);
    kernel_tests::kassert_eq!(rw.uxn(), 1);

    mapper
        .remap(va, PAGE_SIZE, MemFlags::user_rx())
        .expect("remap probe page to UserRX");

    let rx = leaf(mapper, va);
    kernel_tests::kassert!(rx.is_page());
    kernel_tests::kassert_eq!(rx.ap(), 0b11);
    kernel_tests::kassert_eq!(rx.af(), 1);
    kernel_tests::kassert_eq!(rx.ng(), 1);
    kernel_tests::kassert_eq!(rx.pxn(), 1);
    kernel_tests::kassert_eq!(rx.uxn(), 0);

    // AS ни разу не активировался, его entries в TLB нет - drop безопасен.
    // Фрейм страницы пришёл от вызывающего, `map_exact`-leaf'ы Drop не
    // возвращает в FA: отдаём его сами.
    drop(user_as);
    let _ = fa.deallocate_frame(frame);
}
