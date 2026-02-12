extern crate alloc;

use crate::driver::{DriverDescriptor, DriverFactory, ProbeContext};
use crate::probe::ProbeResult;
use crate::{DeviceNode, ProbeError};
use alloc::boxed::Box;
use alloc::collections::BTreeMap;
use alloc::vec::Vec;

/// Функция пробирования runtime-драйвера.
pub type ProbeFn<N> = fn(&mut ProbeContext<N>) -> ProbeResult;

/// Дескриптор runtime-драйвера.
pub type DriverInfo<P> = DriverDescriptor<P>;

pub struct DriverScanner {
    pub(crate) handles: BTreeMap<usize, Box<dyn DriverFactory>>,
}

impl DriverScanner {
    pub fn new() -> Self {
        Self {
            handles: BTreeMap::new(),
        }
    }

    /// Обходит дерево, подбирает и инициализирует runtime-драйверы.
    pub fn scan_and_probe<N, P>(&mut self, root: N, drivers: &[DriverInfo<P>])
    where
        N: DeviceNode<Id = usize>,
        P: Copy + Fn(&mut ProbeContext<N>) -> ProbeResult,
    {
        let mut paths = Vec::<N>::new();
        self.visit_node(&mut paths, root, drivers);
    }

    fn visit_node<N, P>(&mut self, paths: &mut Vec<N>, node: N, drivers: &[DriverInfo<P>])
    where
        N: DeviceNode<Id = usize>,
        P: Copy + Fn(&mut ProbeContext<N>) -> ProbeResult,
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
        drivers: &[DriverInfo<P>],
    ) -> Result<(), ProbeError>
    where
        N: DeviceNode<Id = usize>,
        P: Copy + Fn(&mut ProbeContext<N>) -> ProbeResult,
    {
        for driver_info in drivers {
            let node = context.node();
            let key = node.id();

            let Ok(driver_factory) = (driver_info.probe)(context) else {
                continue;
            };

            if let alloc::collections::btree_map::Entry::Vacant(entry) = self.handles.entry(key) {
                entry.insert(driver_factory);

                return Ok(());
            }
        }

        Err(ProbeError::Unsupported("no driver"))
    }
}

impl Default for DriverScanner {
    fn default() -> Self {
        Self::new()
    }
}

impl IntoIterator for DriverScanner {
    type Item = Box<dyn DriverFactory>;
    type IntoIter = alloc::collections::btree_map::IntoValues<usize, Box<dyn DriverFactory>>;

    fn into_iter(self) -> Self::IntoIter {
        self.handles.into_values()
    }
}
