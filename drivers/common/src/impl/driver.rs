extern crate alloc;

use alloc::vec::Vec;
use interrupts::{IrqHandler, IrqNumber, IrqRegistrationError, IrqRegistrationToken};

use crate::driver::{DriverContext, ProbeContext, InitOps};
use crate::probe::MmioAddress;
use crate::tree::DeviceNode;

impl<N: DeviceNode> ProbeContext<N> {
    pub fn new(node: N, hierarchy: Vec<N>) -> Self {
        Self { node, hierarchy }
    }

    pub fn node(&self) -> &N {
        &self.node
    }

    pub fn hierarchy(&self) -> &[N] {
        &self.hierarchy
    }

    pub fn parent(&self, node: &N) -> Option<&N> {
        self.hierarchy
            .iter()
            .enumerate()
            .find(|(_, n)| n.id() == node.id())
            .and_then(|(index, _)| index.checked_sub(1))
            .and_then(|index| self.hierarchy.get(index))
    }

    pub fn fold<F, S>(&self, fold: F) -> S
    where
        F: Fn(Option<&N>, &N, &ProbeContext<N>) -> Option<S>,
        S: core::iter::Sum,
    {
        self.hierarchy
            .iter()
            .enumerate()
            .filter_map(|(index, node)| {
                let parent = index.checked_sub(1).and_then(|idx| self.hierarchy.get(idx));
                fold(parent, node, self)
            })
            .sum::<S>()
    }
}

impl<'a> DriverContext<'a> {
    pub fn new(ops: &'a mut dyn InitOps) -> Self {
        Self { ops }
    }

    /// Применяет маппинг MMIO-региона.
    pub fn map_mmio(&mut self, address: MmioAddress, size: usize) -> Result<(), &'static str> {
        debug_assert_eq!(address % 4096, 0);
        self.ops.map_mmio(address, size)
    }

    /// Регистрирует IRQ-обработчик.
    pub fn register_irq(
        &mut self,
        irq: IrqNumber,
        handler: &'static dyn IrqHandler,
    ) -> Result<IrqRegistrationToken, IrqRegistrationError> {
        self.ops.register_irq(irq, handler)
    }
}
