extern crate alloc;

use alloc::{boxed::Box, collections::BTreeMap, vec::Vec};

use crate::{
    DeviceNode,
    driver::{DriverDescriptor, DriverFactory},
    probe::{ProbeContext, ProbeError, ProbeResult},
};

/// Функция пробирования runtime-драйвера.
pub type ProbeFn<N> = fn(&mut ProbeContext<N>) -> ProbeResult;

/// Дескриптор runtime-драйвера.
pub type DriverInfo<P> = DriverDescriptor<P>;

pub const MAX_DEVICE_TREE_DEPTH: usize = 128;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ScanError {
    TreeTooDeep {
        limit: usize,
        attempted_depth: usize,
    },
}

pub struct DriverHandle {
    pub name: &'static str,
    pub factory: Box<dyn DriverFactory>,
}

pub struct EmbeddedDriversScanner {
    pub(crate) handles: BTreeMap<usize, DriverHandle>,
}

impl EmbeddedDriversScanner {
    pub fn new() -> Self {
        Self {
            handles: BTreeMap::new(),
        }
    }

    /// Обходит дерево, подбирает и инициализирует runtime-драйверы.
    pub fn scan_and_probe<N, P>(
        &mut self,
        root: N,
        drivers: &[DriverInfo<P>],
    ) -> Result<(), ScanError>
    where
        N: DeviceNode<Id = usize>,
        P: Copy + Fn(&mut ProbeContext<N>) -> ProbeResult,
    {
        let mut paths = Vec::<N>::new();
        self.visit_node(&mut paths, root, drivers)
    }

    fn visit_node<N, P>(
        &mut self,
        paths: &mut Vec<N>,
        node: N,
        drivers: &[DriverInfo<P>],
    ) -> Result<(), ScanError>
    where
        N: DeviceNode<Id = usize>,
        P: Copy + Fn(&mut ProbeContext<N>) -> ProbeResult,
    {
        if paths.len() >= MAX_DEVICE_TREE_DEPTH {
            return Err(ScanError::TreeTooDeep {
                limit: MAX_DEVICE_TREE_DEPTH,
                attempted_depth: paths.len() + 1,
            });
        }

        paths.push(node);

        let mut context = ProbeContext::new(node, paths.clone());
        self.try_probe(&mut context, drivers).ok();

        for child in node.children() {
            if let Err(err) = self.visit_node(paths, child, drivers) {
                paths.pop();
                return Err(err);
            }
        }

        paths.pop();
        Ok(())
    }

    fn try_probe<N, P>(
        &mut self,
        context: &mut ProbeContext<N>,
        drivers: &[DriverDescriptor<P>],
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
                entry.insert(DriverHandle {
                    name: driver_info.name,
                    factory: driver_factory,
                });

                return Ok(());
            }
        }

        Err(ProbeError::Unsupported("no driver"))
    }
}

impl Default for EmbeddedDriversScanner {
    fn default() -> Self {
        Self::new()
    }
}

impl IntoIterator for EmbeddedDriversScanner {
    type Item = DriverHandle;
    type IntoIter = alloc::collections::btree_map::IntoValues<usize, DriverHandle>;

    fn into_iter(self) -> Self::IntoIter {
        self.handles.into_values()
    }
}
