extern crate alloc;

use crate::driver::{DriverDescriptor, ProbeContext};
use crate::{DeviceNode, MmioAddress, ProbeError};
use alloc::boxed::Box;
use alloc::collections::BTreeMap;
use alloc::vec::Vec;
use io::writer::Writer;

pub type EarlyProbeResult = Result<Box<dyn EarlyDriver>, ProbeError>;

/// Функция пробирования раннего драйвера.
pub type EarlyProbeFn<N> = fn(&mut ProbeContext<N>) -> EarlyProbeResult;

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
    fn map_mmio(&mut self, address: MmioAddress);
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

/// Запрос на маппинг MMIO-региона от драйвера.
#[derive(Debug, Clone, Copy)]
pub struct MmioRequest {
    /// Базовый адрес региона.
    pub address: MmioAddress,
}

impl<'a> EarlyDriverContext<'a> {
    /// Создаёт новый ранний контекст.
    pub fn new(ops: &'a mut dyn EarlyInitOps) -> Self {
        Self { ops }
    }

    /// Запрашивает маппинг MMIO-региона.
    pub fn map_mmio(&mut self, address: MmioAddress) {
        debug_assert_eq!(address.base() % 4096, 0);
        self.ops.map_mmio(address);
    }
}

impl<Id> Default for EarlyDriverRegistry<Id>
where
    Id: Ord + Clone,
{
    fn default() -> Self {
        Self::new()
    }
}

impl<Id> EarlyDriverRegistry<Id>
where
    Id: Ord + Clone,
{
    pub fn new() -> Self {
        Self {
            handles: BTreeMap::new(),
            collector: EarlyRequestCollector::new(),
        }
    }

    pub fn mmio_region_requests(&self) -> &[MmioRequest] {
        self.collector.mmio_region_requests()
    }

    pub fn get(&self, key: &Id) -> Option<&dyn EarlyDriver> {
        self.handles.get(key).map(|b| b.as_ref())
    }

    /// Обходит дерево, подбирает и инициализирует ранние драйверы.
    pub fn scan_and_probe<N, P>(&mut self, root: N, drivers: &[EarlyDriverInfo<P>])
    where
        N: DeviceNode<Id = Id>,
        P: Copy + Fn(&mut ProbeContext<N>) -> EarlyProbeResult,
    {
        let mut paths = Vec::<N>::new();
        self.visit_node(&mut paths, root, drivers);
    }

    fn visit_node<N, P>(&mut self, paths: &mut Vec<N>, node: N, drivers: &[EarlyDriverInfo<P>])
    where
        N: DeviceNode<Id = Id>,
        P: Copy + Fn(&mut ProbeContext<N>) -> EarlyProbeResult,
    {
        paths.push(node);

        let mut context = ProbeContext::new(node, paths.clone());
        self.try_probe(&mut context, drivers).ok();

        for child in node.children() {
            self.visit_node(paths, child, drivers);
        }

        paths.pop();
    }

    fn try_probe<N, P>(
        &mut self,
        context: &mut ProbeContext<N>,
        drivers: &[EarlyDriverInfo<P>],
    ) -> Result<(), ProbeError>
    where
        N: DeviceNode<Id = Id>,
        P: Copy + Fn(&mut ProbeContext<N>) -> EarlyProbeResult,
    {
        for driver_info in drivers {
            let node = context.node();
            let key = node.id();

            let Ok(driver) = (driver_info.probe)(context) else {
                continue;
            };

            let init_context = &mut EarlyDriverContext::new(&mut self.collector);

            if let alloc::collections::btree_map::Entry::Vacant(entry) = self.handles.entry(key)
                && driver.init(init_context).is_ok()
            {
                entry.insert(driver);
                return Ok(());
            }
        }

        Err(ProbeError::Unsupported("no driver"))
    }
}

impl Default for EarlyRequestCollector {
    fn default() -> Self {
        Self::new()
    }
}

impl EarlyRequestCollector {
    /// Создаёт пустой сборщик ранних запросов.
    pub fn new() -> Self {
        Self {
            mmio_requests: alloc::vec![],
        }
    }

    /// Возвращает собранные MMIO-запросы.
    pub fn mmio_region_requests(&self) -> &[MmioRequest] {
        &self.mmio_requests
    }
}

impl EarlyInitOps for EarlyRequestCollector {
    fn map_mmio(&mut self, address: MmioAddress) {
        self.mmio_requests.push(MmioRequest { address });
    }
}
