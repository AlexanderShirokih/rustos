extern crate alloc;

use alloc::boxed::Box;
use alloc::collections::BTreeMap;
use interrupts::{IrqHandler, IrqNumber, IrqRegistrationError, IrqRegistrationToken};

use crate::driver::{Driver, DriverDescriptor, ProbeContext};
use crate::probe::ProbeResult;

/// Функция пробирования runtime-драйвера.
pub type ProbeFn<N> = fn(&mut ProbeContext<N>) -> ProbeResult<Box<dyn Driver>>;

/// Дескриптор runtime-драйвера.
pub type DriverInfo<P> = DriverDescriptor<P>;

/// Реестр runtime-драйверов устройств.
pub struct RuntimeDriverRegistry<Id> {
    /// Инициализированные драйверы, индексированные по идентификатору узла.
    pub(crate) handles: BTreeMap<Id, Box<dyn Driver>>,
}

/// Runtime-реализация операций инициализации, применяющая все запросы немедленно.
pub struct RuntimeRequestApplier<'a> {
    pub(crate) mapper: &'a mut dyn FnMut(usize, usize) -> Result<(), &'static str>,
    pub(crate) irq_registrar:
        &'a mut dyn FnMut(
            IrqNumber,
            &'static dyn IrqHandler,
        ) -> Result<IrqRegistrationToken, IrqRegistrationError>,
}
