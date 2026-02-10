extern crate alloc;

use crate::driver::{DriverDescriptor, ProbeContext};
use crate::probe::ProbeResult;
use crate::{MmioAddress, MmioRequest};
use alloc::boxed::Box;
use alloc::collections::BTreeMap;
use alloc::vec::Vec;
use io::writer::Writer;

/// Функция пробирования раннего драйвера.
pub type EarlyProbeFn<N> = fn(&mut ProbeContext<N>) -> ProbeResult<Box<dyn EarlyDriver>>;

/// Трейт раннего драйвера устройства.
pub trait EarlyDriver {
    /// Инициализирует драйвер в раннем контексте.
    fn init(&self, context: &mut EarlyDriverContext) -> Result<(), &'static str>;

    /// Возвращает writer для вывода, если устройство поддерживает вывод.
    fn output(&self) -> Option<Box<dyn Writer + Sync + '_>> {
        None
    }
}
/// Контекст ранней инициализации драйвера.
pub struct EarlyDriverContext<'a> {
    pub(crate) ops: &'a mut dyn EarlyInitOps,
}

/// Операции ранней фазы инициализации драйверов.
pub trait EarlyInitOps {
    /// Запрашивает маппинг MMIO-региона.
    fn map_mmio(&mut self, address: MmioAddress, size: usize);
}

/// Дескриптор раннего драйвера.
pub type EarlyDriverInfo<P> = DriverDescriptor<P>;

/// Реестр ранних драйверов устройств.
pub struct EarlyDriverRegistry<Id> {
    /// Инициализированные драйверы, индексированные по идентификатору узла.
    pub(crate) handles: BTreeMap<Id, Box<dyn EarlyDriver>>,
    /// Сборщик ранних запросов инициализации.
    pub(crate) collector: EarlyRequestCollector,
}

/// Сборщик запросов ранней фазы.
pub(crate) struct EarlyRequestCollector {
    pub mmio_requests: Vec<MmioRequest>,
}
