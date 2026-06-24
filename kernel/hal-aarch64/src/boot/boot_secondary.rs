//! Инициализация secondary (вторичных) ядер процессора.
//!
//! Заглушка: вторичные ядра находятся в режиме ожидания.
//!
//! `secondary_main` помечена `#[unsafe(no_mangle)]` + `extern "C"` как точка
//! входа для secondary-ядер из ASM, поэтому не "dead code" для линкера.

use core::hint::spin_loop;

/// Точка входа для secondary ядер.
#[unsafe(no_mangle)]
pub extern "C" fn secondary_main() -> ! {
    loop {
        spin_loop();
    }
}
