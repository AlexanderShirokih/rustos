//! Минимальная проверка работоспособности харнесса.

use test_harness_qemu::register_test;

fn smoke() {
    let x = core::hint::black_box(2);
    test_harness_qemu::kassert_eq!(x + x, 4);
}

register_test!(SMOKE_TEST, "smoke", smoke);
