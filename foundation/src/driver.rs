//! Трейт драйвера устройства.

extern crate alloc;

use alloc::boxed::Box;
use alloc::vec::Vec;
use io::writer::Writer;

use crate::probe::{MmioAddress, MmioRequest};

/// Трейт драйвера устройства.
///
/// Реализуется конкретными драйверами для предоставления
/// унифицированного интерфейса инициализации и доступа к устройству.
pub trait Driver {
    /// Инициализирует драйвер в заданном контексте.
    fn init(&self, context: &mut DriverContext) -> Result<(), &'static str>;

    /// Возвращает writer для вывода, если устройство поддерживает вывод.
    fn output(&self) -> Option<Box<dyn Writer + Sync + '_>> {
        None
    }
}

/// Контекст инициализации драйвера.
pub struct DriverContext<'a> {
    /// Список запросов на маппинг MMIO.
    mmio_requests: &'a mut Vec<MmioRequest>,
}

impl<'a> DriverContext<'a> {
    /// Создаёт новый контекст с указанным списком запросов MMIO.
    pub fn new(mmio_requests: &'a mut Vec<MmioRequest>) -> Self {
        Self { mmio_requests }
    }

    /// Запрашивает маппинг MMIO-региона.
    pub fn request_mmio(&mut self, address: MmioAddress, size: usize) {
        debug_assert_eq!(address % 4096, 0);

        self.mmio_requests.push(MmioRequest {
            base: address,
            size,
        })
    }
}
