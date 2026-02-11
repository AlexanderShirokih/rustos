extern crate alloc;

use crate::registry::InitOps;
use crate::tree::DeviceNode;
use crate::{MmioAddress, MmioBound, MmioBoundError};
use alloc::boxed::Box;
use alloc::string::String;
use alloc::vec::Vec;
use interrupts::{IrqBound, IrqHandler, IrqNumber, IrqRegistrationError};
use io::writer::Writer;
use memory::mem_flags::{DeviceMemoryPermission, Owners};

/// Контекст пробирования устройства для произвольного типа узла
pub struct ProbeContext<N: DeviceNode> {
    /// Узел, который пробируется.
    pub(crate) node: N,
    /// Иерархия узлов от корня до текущего.
    pub(crate) hierarchy: Vec<N>,
}

/// Дескриптор драйвера, связывающий имя и probe-функцию.
#[repr(C)]
pub struct DriverDescriptor<P> {
    /// Имя драйвера.
    pub name: &'static str,
    /// Функция пробирования.
    pub probe: P,
}

/// Контекст инициализации драйвера.
pub struct DriverContext<'a> {
    pub(crate) ops: &'a dyn InitOps,
}

pub trait DriverFactory {
    fn create(&self, context: &mut DriverContext) -> Result<Box<dyn Driver>, String>;
}

/// Трейт драйвера устройства
pub trait Driver {
    fn run(&mut self) -> Result<(), String> {
        Ok(())
    }

    /// Возвращает writer для вывода, если устройство поддерживает вывод.
    fn output(&self) -> Option<Box<dyn Writer + Sync + '_>> {
        None
    }
}

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
    pub fn new(ops: &'a dyn InitOps) -> Self {
        Self { ops }
    }

    /// Применяет маппинг MMIO-региона.
    pub fn register_mmio(
        &mut self,
        address: MmioAddress,
        permissions: Owners<DeviceMemoryPermission>,
    ) -> Result<MmioBound, MmioBoundError> {
        self.ops.register_mmio(address, permissions)
    }

    /// Регистрирует IRQ-обработчик.
    pub fn register_irq(
        &mut self,
        irq: IrqNumber,
        handler: &'static dyn IrqHandler,
    ) -> Result<IrqBound, IrqRegistrationError> {
        self.ops.register_irq(irq, handler)
    }
}
