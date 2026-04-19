//! Инициализация secondary (вторичных) ядер процессора.
//!
//! Заглушка: вторичные ядра находятся в режиме ожидания. Функция помечена
//! `#[unsafe(no_mangle)]` + `extern "C"` для использования из ASM в будущем
//! SMP-bringup сценарии - поэтому она не "dead code" с точки зрения линкера.

use core::hint::spin_loop;

/// Точка входа для secondary ядер.
#[unsafe(no_mangle)]
pub extern "C" fn secondary_main() -> ! {
    loop {
        spin_loop();
    }
}
