//! Система выключения машины.

use spin::Once;

static EXIT: Once<fn(i32) -> !> = Once::new();

/// Регистрирует платформенный обработчик выключения.
pub fn install(exit: fn(i32) -> !) {
    EXIT.call_once(|| exit);
}

/// Выключает машину с кодом `code`. Без установленной реализации уходит в вечный halt-цикл.
pub fn system_off(code: i32) -> ! {
    if let Some(exit) = EXIT.get() {
        exit(code);
    }
    loop {
        core::hint::spin_loop();
    }
}
