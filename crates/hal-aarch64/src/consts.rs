//! Файл с глобальными константами ядра.

/// База higher-half (верхняя половина адресного пространства).
pub const HIGHER_HALF_BASE: usize = 0xFFFF_FF80_0000_0000;

/// База heap-арены, выше окна линейной PA->VA-карты
/// (`HIGHER_HALF_BASE + region.start`). Платформам с RAM > 480 GB
/// потребуется пересчёт.
pub const KHEAP_BASE: usize = 0xFFFF_FFF8_0000_0000;

/// VA-cap heap-арены. Физическая RAM приходит лениво при expand'е.
pub const KHEAP_MAX_SIZE: usize = 16 * 1024 * 1024 * 1024;

/// База kernel-MMIO арены - окно kernel-VA, из которого `MmioServiceImpl`
/// выдаёт страницы под маппинг устройственных регистров. Лежит сразу за
/// heap-ареной (`KHEAP_BASE + KHEAP_MAX_SIZE`).
pub const KMMIO_BASE: usize = 0xFFFF_FFFC_0000_0000;

/// VA-cap kernel-MMIO арены.
pub const KMMIO_MAX_SIZE: usize = 4 * 1024 * 1024 * 1024;
