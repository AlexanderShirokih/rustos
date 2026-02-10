extern crate alloc;

use alloc::boxed::Box;
use alloc::vec::Vec;
use interrupts::{IrqHandler, IrqNumber, IrqRegistrationError, IrqRegistrationToken};

use crate::driver::{Driver, DriverContext, InitOps, ProbeContext};
use crate::probe::ProbeError;
use crate::runtime::{DriverInfo, RuntimeDriverRegistry, RuntimeRequestApplier};
use crate::tree::DeviceNode;

impl<Id> Default for RuntimeDriverRegistry<Id>
where
    Id: Ord + Clone,
{
    fn default() -> Self {
        Self::new()
    }
}

impl<Id> RuntimeDriverRegistry<Id>
where
    Id: Ord + Clone,
{
    pub fn new() -> Self {
        Self {
            handles: alloc::collections::BTreeMap::new(),
        }
    }

    pub fn get(&self, key: &Id) -> Option<&dyn Driver> {
        self.handles.get(key).map(|b| b.as_ref())
    }

    /// Обходит дерево, подбирает и инициализирует runtime-драйверы.
    pub fn scan_and_probe<N, P>(
        &mut self,
        root: N,
        drivers: &[DriverInfo<P>],
        init_ops: &mut dyn InitOps,
    ) where
        N: DeviceNode<Id = Id>,
        P: Copy + Fn(&mut ProbeContext<N>) -> crate::probe::ProbeResult<Box<dyn Driver>>,
    {
        let mut paths = Vec::<N>::new();
        self.visit_node(&mut paths, root, drivers, init_ops);
    }

    fn visit_node<N, P>(
        &mut self,
        paths: &mut Vec<N>,
        node: N,
        drivers: &[DriverInfo<P>],
        init_ops: &mut dyn InitOps,
    ) where
        N: DeviceNode<Id = Id>,
        P: Copy + Fn(&mut ProbeContext<N>) -> crate::probe::ProbeResult<Box<dyn Driver>>,
    {
        paths.push(node);

        let mut context = ProbeContext::new(node, paths.clone());
        self.try_probe(&mut context, drivers, init_ops).ok();

        for child in node.children() {
            self.visit_node(paths, child, drivers, init_ops);
        }

        paths.pop();
    }

    fn try_probe<N, P>(
        &mut self,
        context: &mut ProbeContext<N>,
        drivers: &[DriverInfo<P>],
        init_ops: &mut dyn InitOps,
    ) -> Result<(), ProbeError>
    where
        N: DeviceNode<Id = Id>,
        P: Copy + Fn(&mut ProbeContext<N>) -> crate::probe::ProbeResult<Box<dyn Driver>>,
    {
        for driver_info in drivers {
            let node = context.node();
            let key = node.id();

            let Ok(driver) = (driver_info.probe)(context) else {
                continue;
            };

            let init_context = &mut DriverContext::new(init_ops);

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

impl<'a> RuntimeRequestApplier<'a> {
    pub fn new(
        mapper: &'a mut dyn FnMut(usize, usize) -> Result<(), &'static str>,
        irq_registrar: &'a mut dyn FnMut(
            IrqNumber,
            &'static dyn IrqHandler,
        )
            -> Result<IrqRegistrationToken, IrqRegistrationError>,
    ) -> Self {
        Self {
            mapper,
            irq_registrar,
        }
    }
}

impl InitOps for RuntimeRequestApplier<'_> {
    fn map_mmio(&mut self, address: usize, size: usize) -> Result<(), &'static str> {
        (self.mapper)(address, size)
    }

    fn register_irq(
        &mut self,
        irq: IrqNumber,
        handler: &'static dyn IrqHandler,
    ) -> Result<IrqRegistrationToken, IrqRegistrationError> {
        (self.irq_registrar)(irq, handler)
    }
}
