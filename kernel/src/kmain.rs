extern crate alloc;

use alloc::boxed::Box;
use alloc::vec::Vec;
use klog::info;

/// Главная функция ядра. Вызывается после инициализации памяти и драйверов.
pub fn kmain() {
    test_allocator();
}

fn test_allocator() {
    info!("Тест аллокатора");

    // Маленький объект
    let small = Box::new(42u64);
    info!("u64: ptr = {:p}, value = {}", small.as_ref(), *small);
    drop(small);

    // Большой объект (2 страницы)
    let huge = Box::new([0xAAu8; 8192]);
    info!("[u8; 8192]: ptr = {:p}", huge.as_ref());
    drop(huge);

    // Vec больше страницы
    let mut vec: Vec<u64> = Vec::with_capacity(600);
    for i in 0..600 {
        vec.push(i);
    }
    info!("Vec<u64>: len = {}, cap = {}, ptr = {:p}", vec.len(), vec.capacity(), vec.as_ptr());
    drop(vec);

    info!("Тест завершен");
}
