use crate::driver::probe::CompatibleList;
use crate::driver::probe::{NodeProbeExt, ProbeError, ProbeResult};
use crate::io::writer::Writer;
use alloc::boxed::Box;
use alloc::collections::BTreeMap;
use alloc::vec::Vec;
use fdt::devicetree::{DeviceTree, Node, NodeKey};
use fdt::devicetreeext::{CellsSize, DeviceTreeExt};

type EarlyDriverId = NodeKey;

pub struct EarlyDriverRegistry {
    handles: BTreeMap<EarlyDriverId, EarlyDriverHandle>,
}

impl EarlyDriverRegistry {
    pub fn new() -> Self {
        Self {
            handles: BTreeMap::new(),
        }
    }

    pub fn take(&mut self, key: &NodeKey) -> Option<EarlyDriverHandle> {
        self.handles.remove(key)
    }

    pub fn scan_and_probe(&mut self, dt: &DeviceTree<'_>) {
        let cells_size = dt.cells_size().unwrap_or_default();

        match dt.root() {
            Some(root) => {
                let mut paths = Vec::<Node>::new();
                self.visit_node(&mut paths, &root, &cells_size);
            }

            None => return,
        }
    }

    fn visit_node<'a>(
        &mut self,
        paths: &mut Vec<Node<'a>>,
        node: &Node<'a>,
        cells_size: &CellsSize,
    ) {
        paths.push(node.clone());

        if node.prop("compatible").is_some() {
            let context = ProbeContext {
                node,
                cells_size,
                hierarchy: paths,
            };

            self.try_probe(&context).ok();
        }

        for child in node.children() {
            self.visit_node(paths, &child, cells_size);
        }

        paths.pop();
    }

    fn try_probe(&mut self, context: &ProbeContext) -> Result<(), ProbeError> {
        for driver in early_drivers() {
            let node = context.node();
            if node.is_compatible_any(driver.compatible) {
                let device = (driver.probe)(context)?;
                self.handles.insert(node.key(), device);
                return Ok(());
            }
        }

        Err(ProbeError::Unsupported("no early driver"))
    }
}

/// Результат probe early драйвера
pub enum EarlyDriverHandle {
    Writer(Box<dyn Writer>),
    Opaque,
}

pub struct ProbeContext<'a> {
    node: &'a Node<'a>,
    hierarchy: &'a Vec<Node<'a>>,
    cells_size: &'a CellsSize,
}

impl ProbeContext<'_> {
    pub fn node(&self) -> &Node<'_> {
        &self.node
    }

    pub fn hierarchy(&self) -> &[Node<'_>] {
        &self.hierarchy
    }

    pub fn fold<F, S>(&self, fold: F) -> S
    where
        F: Fn(&Node, &ProbeContext) -> Option<S>,
        S: core::iter::Sum,
    {
        self.hierarchy
            .iter()
            .filter_map(|node| fold(node, self))
            .sum::<S>()
    }

    pub fn cells_size(&self) -> CellsSize {
        self.cells_size.clone()
    }
}

pub type EarlyProbeFn = fn(&ProbeContext) -> ProbeResult<EarlyDriverHandle>;

#[repr(C)]
pub struct EarlyDriverInfo {
    pub name: &'static str,
    pub compatible: CompatibleList,
    pub probe: EarlyProbeFn,
}

fn early_drivers() -> &'static [EarlyDriverInfo] {
    #[allow(improper_ctypes)]
    unsafe extern "C" {
        static __drivers_early_start: EarlyDriverInfo;
        static __drivers_early_end: EarlyDriverInfo;
    }

    unsafe {
        let start = &__drivers_early_start as *const EarlyDriverInfo;
        let end = &__drivers_early_end as *const EarlyDriverInfo;
        let length = end.offset_from(start) as usize;
        core::slice::from_raw_parts(start, length)
    }
}

#[macro_export]
macro_rules! register_early_driver {
    ($symbol:ident, compatible = $compatible:expr, probe = $probe:expr) => {
        #[unsafe(link_section = ".drivers.early")]
        #[used]
        static $symbol: $crate::driver::early::EarlyDriverInfo =
            $crate::driver::early::EarlyDriverInfo {
                name: stringify!($symbol),
                compatible: $compatible,
                probe: $probe,
            };
    };
}
