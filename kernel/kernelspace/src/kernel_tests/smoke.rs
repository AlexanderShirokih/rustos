//! Минимальная проверка работоспособности харнесса.

use kernel_tests::kernel_test;

#[kernel_test]
fn smoke() {
    let x = core::hint::black_box(2);
    kernel_tests::kassert_eq!(x + x, 4);
}
