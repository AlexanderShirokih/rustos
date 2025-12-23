use crate::driver::probe::{CompatibleList, MmioAddress, MmioRequest};
use crate::driver::probe::{NodeProbeExt, ProbeError, ProbeResult};
use alloc::boxed::Box;
use alloc::collections::BTreeMap;
use alloc::vec;
use alloc::vec::Vec;
use fdt::devicetree::{DeviceTree, Node, NodeKey};
use io::writer::Writer;

type EarlyDriverId = NodeKey;

pub struct EarlyDriverRegistry {
    handles: BTreeMap<EarlyDriverId, Box<dyn EarlyDriver>>,
    mmio_requests: Vec<MmioRequest>,
}

impl EarlyDriverRegistry {
    pub fn new() -> Self {
        Self {
            handles: BTreeMap::new(),
            mmio_requests: vec![],
        }
    }

    pub fn mmio_region_requests(&self) -> &[MmioRequest] {
        &self.mmio_requests
    }

    pub fn get(&self, key: &NodeKey) -> Option<&Box<dyn EarlyDriver>> {
        self.handles.get(key)
    }

    pub fn scan_and_probe(&mut self, dt: &DeviceTree<'_>) {
        match dt.root() {
            Some(root) => {
                let mut paths = Vec::<Node>::new();
                self.visit_node(&mut paths, &root);
            }

            None => return,
        }
    }

    fn visit_node<'a>(&mut self, paths: &mut Vec<Node<'a>>, node: &Node<'a>) {
        paths.push(node.clone());

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
        for driver_info in early_drivers() {
            let node = context.node();
            let key = node.key();
            if node.is_compatible_any(driver_info.compatible) {
                let driver = (driver_info.probe)(context)?;
                let context = &mut EarlyDriverContext {
                    mmio_requests: &mut self.mmio_requests,
                };

                if !self.handles.contains_key(&key) {
                    if driver.init(context).is_ok() {
                        self.handles.insert(key, driver);
                    }
                }
            }
        }

        Err(ProbeError::Unsupported("no early driver"))
    }
}

pub struct EarlyDriverContext<'a> {
    mmio_requests: &'a mut Vec<MmioRequest>,
}

impl EarlyDriverContext<'_> {
    pub fn request_mmio(&mut self, address: MmioAddress, size: usize) {
        debug_assert_eq!(address % 4096, 0);

        self.mmio_requests.push(MmioRequest {
            base: address,
            size,
        })
    }
}

pub trait EarlyDriver {
    fn init(&self, context: &mut EarlyDriverContext) -> Result<(), &'static str>;

    fn output(&self) -> Option<Box<dyn Writer + Sync + '_>> {
        None
    }
}

pub struct ProbeContext<'a> {
    node: &'a Node<'a>,
    hierarchy: &'a [Node<'a>],
}

impl<'a> ProbeContext<'a> {
    pub fn node(&self) -> &Node<'_> {
        &self.node
    }

    pub fn hierarchy(&self) -> &[Node<'_>] {
        &self.hierarchy
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

pub type EarlyProbeFn = fn(&mut ProbeContext) -> ProbeResult<Box<dyn EarlyDriver>>;

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
