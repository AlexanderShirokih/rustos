//! Инициализация secondary (вторичных) ядер процессора.
//!
//! Заглушка: вторичные ядра находятся в режиме ожидания.

use core::hint::spin_loop;

/// Точка входа для secondary ядер.
pub fn secondary_main() -> ! {
    loop {
        spin_loop();
    }
}
