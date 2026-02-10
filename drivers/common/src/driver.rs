extern crate alloc;

use alloc::boxed::Box;
use alloc::vec::Vec;
use interrupts::{IrqHandler, IrqNumber, IrqRegistrationError, IrqRegistrationToken};
use io::writer::Writer;

use crate::probe::MmioAddress;
use crate::tree::DeviceNode;

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

/// Операции runtime-фазы инициализации драйверов.
pub trait InitOps {
    /// Применяет маппинг MMIO-региона.
    fn map_mmio(&mut self, address: MmioAddress, size: usize) -> Result<(), &'static str>;

    /// Регистрирует IRQ-обработчик.
    fn register_irq(
        &mut self,
        irq: IrqNumber,
        handler: &'static dyn IrqHandler,
    ) -> Result<IrqRegistrationToken, IrqRegistrationError>;
}

/// Контекст инициализации драйвера.
pub struct DriverContext<'a> {
    pub(crate) ops: &'a mut dyn InitOps,
}

/// Трейт драйвера устройства
pub trait Driver {
    fn init(&self, context: &mut DriverContext) -> Result<(), &'static str>;

    /// Возвращает writer для вывода, если устройство поддерживает вывод.
    fn output(&self) -> Option<Box<dyn Writer + Sync + '_>> {
        None
    }
}
