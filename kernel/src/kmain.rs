extern crate alloc;

use alloc::boxed::Box;
use alloc::vec::Vec;
use klog::info;

pub fn kmain() {
    info!("Hello, world!");

    test_allocator();
}

/// Тест аллокатора памяти с объектами разного размера
fn test_allocator() {
    info!("=== Тест аллокатора памяти ===");

    // --- Тест 1: маленький объект (8 байт) ---
    info!("Выделяем u64...");
    let small = Box::new(42u64);
    info!("  u64 ptr = {:p}, value = {}", small.as_ref(), *small);
    drop(small);
    info!("  u64 освобожден");

    // --- Тест 2: средний объект (структура ~32 байта) ---
    #[repr(C)]
    struct Medium {
        a: u64,
        b: u64,
        c: u64,
        d: u64,
    }

    info!("Выделяем Medium (32 байта)...");
    let medium = Box::new(Medium {
        a: 1,
        b: 2,
        c: 3,
        d: 4,
    });
    info!("  Medium ptr = {:p}", medium.as_ref());
    drop(medium);
    info!("  Medium освобожден");

    // --- Тест 3: большой объект (массив 256 байт) ---
    info!("Выделяем [u8; 256]...");
    let large = Box::new([0xAAu8; 256]);
    info!("  [u8; 256] ptr = {:p}", large.as_ref());
    drop(large);
    info!("  [u8; 256] освобожден");

    // --- Тест 4: Vec с динамическим ростом ---
    info!("Выделяем Vec и добавляем элементы...");
    let mut vec: Vec<u32> = Vec::new();
    info!("  Vec создан, capacity = {}", vec.capacity());

    for i in 0..10 {
        vec.push(i);
    }
    info!("  Vec после push: len = {}, cap = {}, ptr = {:p}", vec.len(), vec.capacity(), vec.as_ptr());
    drop(vec);
    info!("  Vec освобожден");

    // --- Тест 5: повторное выделение после освобождения ---
    info!("Повторное выделение после освобождения...");

    let a = Box::new(100u64);
    let ptr_a = a.as_ref() as *const u64;
    info!("  a: ptr = {:p}", ptr_a);
    drop(a);

    let b = Box::new(200u64);
    let ptr_b = b.as_ref() as *const u64;
    info!("  b: ptr = {:p}", ptr_b);

    if ptr_a == ptr_b {
        info!("  Память переиспользована!");
    } else {
        info!("  Выделена новая память");
    }
    drop(b);

    // --- Тест 6: несколько объектов одновременно ---
    info!("Выделяем несколько объектов одновременно...");
    let obj1 = Box::new([1u8; 16]);
    let obj2 = Box::new([2u8; 64]);
    let obj3 = Box::new([3u8; 128]);

    info!("  obj1 (16 байт):  {:p}", obj1.as_ref());
    info!("  obj2 (64 байта): {:p}", obj2.as_ref());
    info!("  obj3 (128 байт): {:p}", obj3.as_ref());

    // Освобождаем в обратном порядке
    drop(obj3);
    info!("  obj3 освобожден");
    drop(obj2);
    info!("  obj2 освобожден");
    drop(obj1);
    info!("  obj1 освобожден");

    info!("=== Тест аллокатора завершен ===");
}
