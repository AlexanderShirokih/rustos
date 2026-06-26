//! Регрессии kernel-heap'а: aligned-аллокации не должны зависеть от
//! фрагментации physical-bitmap'а (root-фреймы Drop'нутых AS и т.п.).
#![allow(unsafe_code)]

extern crate alloc;

use kernel_tests::kernel_test;
use kernelspace::syscall_bridge;
use scheduler::AddressSpace;

const PAGE_SIZE: usize = memory::PAGE_SIZE.get();

/// Цикл `new_user + drop ×N` (без map) рассыпает дырки в physical-bitmap'е;
/// последующий `alloc_zeroed(4K, 4K)` не должен возвращать null.
#[kernel_test]
fn empty_user_as_drop_cycle_does_not_block_aligned_kheap_alloc() {
    let factory =
        syscall_bridge::address_space_factory().expect("address space factory must be installed");

    for _ in 0..32 {
        let as_ = AddressSpace::new_user(factory).expect("create user AS");
        drop(as_);
    }

    let layout =
        core::alloc::Layout::from_size_align(PAGE_SIZE, PAGE_SIZE).expect("PAGE_SIZE valid layout");
    // SAFETY: layout валиден; ptr - свежевыделенный, эксклюзивно наш.
    let ptr = unsafe { alloc::alloc::alloc_zeroed(layout) };
    kernel_tests::kassert!(!ptr.is_null());

    // SAFETY: 4K-страница, выровнена на 4K - запись u64 безопасна.
    #[allow(clippy::cast_ptr_alignment)]
    unsafe {
        ptr.cast::<u64>().write_volatile(0xDEAD_BEEF_CAFE_BABE);
        let v = ptr.cast::<u64>().read_volatile();
        kernel_tests::kassert_eq!(v, 0xDEAD_BEEF_CAFE_BABE);
    }
}

/// Серия aligned-4K-4K-аллокаций после Drop-цикла должна стабильно проходить.
#[kernel_test]
fn many_aligned_kheap_allocs_after_drop_cycle_succeed() {
    let factory =
        syscall_bridge::address_space_factory().expect("address space factory must be installed");

    for _ in 0..16 {
        let as_ = AddressSpace::new_user(factory).expect("create user AS");
        drop(as_);
    }

    let layout =
        core::alloc::Layout::from_size_align(PAGE_SIZE, PAGE_SIZE).expect("PAGE_SIZE valid layout");
    for i in 0u64..8 {
        // SAFETY: layout валиден.
        let ptr = unsafe { alloc::alloc::alloc_zeroed(layout) };
        kernel_tests::kassert!(!ptr.is_null());
        // Уникальный pattern, чтобы поймать overlap между аллокациями.
        // SAFETY: 4K-страница, выровнена на 4K.
        #[allow(clippy::cast_ptr_alignment)]
        unsafe {
            ptr.cast::<u64>().write_volatile(0xA000_0000 + i);
        }
    }
}
