use alloc::boxed::Box;
use alloc::collections::BTreeMap;
use alloc::vec;
use alloc::vec::Vec;
use fdt::devicetree::{DeviceTree, Node, NodeKey};
use foundation::{Driver, DriverContext, MmioRequest, ProbeError, ProbeResult};

use crate::probe::{CompatibleList, NodeProbeExt};

/// Идентификатор драйвера (ключ узла в DeviceTree).
type DriverId = NodeKey;

/// Реестр драйверов устройств.
///
/// Хранит инициализированные драйверы и запросы на маппинг MMIO-регионов.
pub struct DriverRegistry {
    /// Инициализированные драйверы, индексированные по ключу узла.
    handles: BTreeMap<DriverId, Box<dyn Driver>>,
    /// Запросы на маппинг MMIO-регионов от драйверов.
    mmio_requests: Vec<MmioRequest>,
}

impl Default for DriverRegistry {
    fn default() -> Self {
        Self::new()
    }
}

impl DriverRegistry {
    pub fn new() -> Self {
        Self {
            handles: BTreeMap::new(),
            mmio_requests: vec![],
        }
    }

    pub fn mmio_region_requests(&self) -> &[MmioRequest] {
        &self.mmio_requests
    }

    pub fn get(&self, key: &NodeKey) -> Option<&dyn Driver> {
        self.handles.get(key).map(|b| b.as_ref())
    }

    pub fn scan_and_probe(&mut self, dt: &DeviceTree<'_>) {
        if let Some(root) = dt.root() {
            let mut paths = Vec::<Node>::new();
            self.visit_node(&mut paths, &root);
        }
    }

    fn visit_node<'a>(&mut self, paths: &mut Vec<Node<'a>>, node: &Node<'a>) {
        paths.push(*node);

        if node.prop("compatible").is_some() {
            let mut context = ProbeContext {
                node,
                hierarchy: paths,
            };

            self.try_probe(&mut context).ok();
        }

        for child in node.children() {
            self.visit_node(paths, &child);
        }

        paths.pop();
    }

    fn try_probe(&mut self, context: &mut ProbeContext) -> Result<(), ProbeError> {
        for driver_info in drivers() {
            let node = context.node();
            let key = node.key();
            if node.is_compatible_any(driver_info.compatible) {
                let driver = (driver_info.probe)(context)?;
                let context = &mut DriverContext::new(&mut self.mmio_requests);

                if let alloc::collections::btree_map::Entry::Vacant(e) = self.handles.entry(key)
                    && driver.init(context).is_ok()
                {
                    e.insert(driver);
                }
            }
        }

        Err(ProbeError::Unsupported("no driver"))
    }
}


/// Контекст пробирования устройства.
pub struct ProbeContext<'a> {
    /// Узел DeviceTree, который пробируется.
    node: &'a Node<'a>,
    /// Иерархия узлов от корня до текущего.
    hierarchy: &'a [Node<'a>],
}

impl<'a> ProbeContext<'a> {
    pub fn node(&self) -> &Node<'_> {
        self.node
    }

    pub fn hierarchy(&self) -> &[Node<'_>] {
        self.hierarchy
    }

    pub fn parent(&self, node: &Node) -> Option<&'a Node<'a>> {
        self.hierarchy
            .iter()
            .enumerate()
            .find(|(_, n)| n.key() == node.key())
            .and_then(|(index, _)| self.hierarchy.get(index - 1))
    }

    pub fn fold<F, S>(&self, fold: F) -> S
    where
        F: Fn(Option<&Node>, &Node, &ProbeContext) -> Option<S>,
        S: core::iter::Sum,
    {
        self.hierarchy
            .iter()
            .enumerate()
            .filter_map(|(index, node)| {
                let parent = self.hierarchy.get(index - 1);
                fold(parent, node, self)
            })
            .sum::<S>()
    }
}

/// Функция пробирования драйвера.
pub type ProbeFn = fn(&mut ProbeContext) -> ProbeResult<Box<dyn Driver>>;

/// Информация о зарегистрированном драйвере.
#[repr(C)]
pub struct DriverInfo {
    /// Имя драйвера.
    pub name: &'static str,
    /// Список совместимых строк из DeviceTree.
    pub compatible: CompatibleList,
    /// Функция пробирования.
    pub probe: ProbeFn,
}

fn drivers() -> &'static [DriverInfo] {
    #[allow(improper_ctypes)]
    unsafe extern "C" {
        static __drivers_early_start: DriverInfo;
        static __drivers_early_end: DriverInfo;
    }

    unsafe {
        let start = &__drivers_early_start as *const DriverInfo;
        let end = &__drivers_early_end as *const DriverInfo;
        let length = end.offset_from(start) as usize;
        core::slice::from_raw_parts(start, length)
    }
}

#[macro_export]
macro_rules! register_driver {
    ($symbol:ident, compatible = $compatible:expr, probe = $probe:expr) => {
        #[unsafe(link_section = ".drivers.early")]
        #[used]
        static $symbol: $crate::driver::DriverInfo = $crate::driver::DriverInfo {
            name: stringify!($symbol),
            compatible: $compatible,
            probe: $probe,
        };
    };
}
