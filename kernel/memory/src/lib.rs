//! Крейт управления памятью ядра.
//!
//! Предоставляет примитивы для работы с физической и виртуальной памятью:
//! аллокаторы фреймов, bump-аллокатор, аллокатор кучи и типы адресов.

#![cfg_attr(not(test), no_std)]
extern crate alloc;

#[cfg(test)]
extern crate std;

use core::num::NonZeroUsize;

pub mod align;
pub mod aligned;
pub mod bump_allocator;
pub mod frame;
pub mod frame_allocator;
mod frame_bitmap;
pub mod kernel_vm_allocator;
pub mod mem_flags;
pub mod memory;
pub mod memory_mapper;
pub mod memory_range;
pub mod physical_address;
pub mod range_allocator;
pub mod region;
pub mod relocatable_ptr;
pub mod user_vm_allocator;
pub mod user_vm_context;
pub mod virtual_address;

#[cfg_attr(not(test), doc(hidden))]
pub use frame_bitmap::FrameBitmap;
pub use mem_flags::MemFlags;
pub use region::{
    AccessMask, BudgetRefund, MemoryBacking, MemoryRegion, MemoryType, RegionCreateError,
    RegionSliceError,
};
pub use relocatable_ptr::RelocatablePtr;
pub use user_vm_allocator::{MappingTag, UserVmAllocator};
pub use user_vm_context::{UserVmContext, WeakUserVmContext};

/// Размер страницы и фрейма в байтах. Выводится из выравнивания page-aligned
/// типа адреса.
pub const PAGE_SIZE: NonZeroUsize =
    NonZeroUsize::new(<physical_address::PageAlignedAddress as aligned::Aligned>::ALIGNMENT)
        .unwrap();

/// Округляет ненулевой размер вверх до кратного [`PAGE_SIZE`]. Ненулевость
/// входа сохраняется типом; `None` только при переполнении `usize`.
pub const fn page_round_up(value: NonZeroUsize) -> Option<NonZeroUsize> {
    let page = PAGE_SIZE.get();
    let rem = value.get() % page;
    if rem == 0 {
        return Some(value);
    }
    // value >= 1 и добивка `page - rem` > 0, поэтому сумма ненулевая;
    // None даёт только переполнение usize.
    match value.get().checked_add(page - rem) {
        Some(rounded) => NonZeroUsize::new(rounded),
        None => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn page_round_up_keeps_aligned_value() {
        assert_eq!(page_round_up(PAGE_SIZE), Some(PAGE_SIZE));
        let two = NonZeroUsize::new(PAGE_SIZE.get() * 2).unwrap();
        assert_eq!(page_round_up(two), Some(two));
    }

    #[test]
    fn page_round_up_rounds_partial_page() {
        let one = NonZeroUsize::new(1).unwrap();
        assert_eq!(page_round_up(one), Some(PAGE_SIZE));
        let over = NonZeroUsize::new(PAGE_SIZE.get() + 1).unwrap();
        assert_eq!(page_round_up(over), NonZeroUsize::new(PAGE_SIZE.get() * 2));
    }

    #[test]
    fn page_round_up_detects_overflow() {
        let huge = NonZeroUsize::new(usize::MAX).unwrap();
        assert_eq!(page_round_up(huge), None);
    }
}
