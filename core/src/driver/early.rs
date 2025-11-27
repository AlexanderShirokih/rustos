use crate::console::Console;
use crate::driver::probe::CompatibleList;
use crate::driver::probe::{NodeProbeExt, ProbeError, ProbeResult};
use alloc::boxed::Box;
use alloc::collections::BTreeMap;
use alloc::string::ToString;
use fdt::devicetree::{DeviceTree, Node};
use fdt::devicetreeext::{CellsSize, DeviceTreeExt};

type EarlyDriverId = alloc::string::String;

pub struct EarlyDriverRegistry {
    handles: BTreeMap<EarlyDriverId, EarlyDriverHandle>,
}

impl<'dt> EarlyDriverRegistry {
    pub fn new() -> Self {
        Self {
            handles: BTreeMap::new(),
        }
    }

    pub fn take(&mut self, name: &str) -> Option<EarlyDriverHandle> {
        self.handles.remove(name)
    }

    pub fn scan_and_probe(&mut self, dt: &DeviceTree) {
        let cells_size = dt.cells_size().unwrap_or_default();

        for node in dt.nodes() {
            if node.prop("compatible").is_none() {
                continue;
            }

            let context = ProbeContext {
                node: &node,
                cells_size,
            };
            if self.try_probe(&context).is_ok() {
                continue;
            }
        }
    }

    fn try_probe(&mut self, context: &ProbeContext) -> Result<(), ProbeError> {
        for driver in early_drivers() {
            let node = context.node();
            if node.is_compatible_any(driver.compatible) {
                let device = (driver.probe)(context)?;
                self.handles.insert(node.name().to_string(), device);
                return Ok(());
            }
        }

        Err(ProbeError::Unsupported("no early driver"))
    }
}

/// Результат probe early драйвера
pub enum EarlyDriverHandle {
    Console(Box<dyn Console>),
    Opaque,
}

pub struct ProbeContext<'a> {
    node: &'a Node<'a>,
    cells_size: CellsSize,
}

impl ProbeContext<'_> {
    pub fn node(&self) -> &Node<'_> {
        &self.node
    }

    pub fn cells_size(&self) -> CellsSize {
        self.cells_size
    }
}

pub type EarlyProbeFn = fn(&ProbeContext<'_>) -> ProbeResult<EarlyDriverHandle>;

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
