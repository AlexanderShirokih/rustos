//! Уговор `log-client-fixture` с прогоном описывает строку, которую фикстура пишет по
//! своей лог-капе, и коды её выхода.

// Модуль подключён к обоим бинарям, и каждый пользуется своим подмножеством.
#![allow(dead_code)]

/// Строка, которую фикстура пишет по своей лог-капе.
pub const FIXTURE_LINE: &str = "log capability works";

/// По лог-капе выдан корневой IrqControl.
pub const EXIT_IRQ_CONTROL: u64 = 1;
/// По лог-капе выдан регион userland-образа.
pub const EXIT_USERLAND_IMAGE: u64 = 2;
/// По лог-капе выдан регион boot FDT.
pub const EXIT_BOOT_FDT: u64 = 3;
/// По лог-капе выдан Device-MMIO регион.
pub const EXIT_DEVICE_MEMORY: u64 = 4;
/// Строка лога по той же капе не прошла.
pub const EXIT_LOG_REFUSED: u64 = 5;
