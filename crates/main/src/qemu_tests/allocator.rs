//! Базовый прогон heap-аллокатора в живом ядре.

extern crate alloc;

use qemu_test_harness::register_test;

fn allocator_basic() {
    use alloc::vec::Vec;

    let mut v: Vec<u32> = Vec::with_capacity(16);
    for i in 0..16 {
        v.push(i);
    }
    qemu_test_harness::kassert_eq!(v.len(), 16);
    qemu_test_harness::kassert_eq!(v[0], 0);
    qemu_test_harness::kassert_eq!(v[15], 15);
}

register_test!(ALLOCATOR_BASIC, "allocator_basic", allocator_basic);
