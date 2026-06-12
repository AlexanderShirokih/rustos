//! Базовый прогон heap-аллокатора в ядре.

extern crate alloc;

use kernel_tests::kernel_test;

#[kernel_test]
fn allocator_basic() {
    use alloc::vec::Vec;

    let mut v: Vec<u32> = Vec::with_capacity(16);
    for i in 0..16 {
        v.push(i);
    }
    kernel_tests::kassert_eq!(v.len(), 16);
    kernel_tests::kassert_eq!(v[0], 0);
    kernel_tests::kassert_eq!(v[15], 15);
}
