//! Минимальная проверка работоспособности харнесса.

#[test_harness::test]
fn smoke() {
    let x = core::hint::black_box(2);
    test_harness::check_eq!(x + x, 4);
}
