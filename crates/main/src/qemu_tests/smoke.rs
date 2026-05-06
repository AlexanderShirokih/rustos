//! Минимальная проверка работоспособности харнесса.

use qemu_test_harness::register_test;

fn smoke() {
    let x = core::hint::black_box(2);
    qemu_test_harness::kassert_eq!(x + x, 4);
}

register_test!(SMOKE_TEST, "smoke", smoke);
