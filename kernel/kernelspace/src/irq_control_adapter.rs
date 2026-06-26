//! Адаптер `capability::InterruptsControl` поверх `drivers_common`-сервиса.
//!
//! Единственное место, где встречаются архитектурно-независимый capability-слой и
//! drivers-common: capability говорит «сырыми» `u16`, адаптер заворачивает их в
//! `IrqNumber`, строит `IrqBinding` с мостом `IrqHandler -> IrqSink` и хранит
//! возвращённый `IrqBound` внутри `IrqBindToken`. Так capability-крейт остаётся
//! без зависимости на drivers-common.

use alloc::{boxed::Box, sync::Arc};

use capability::{InterruptsControl, IpcError, IrqBindToken, IrqSink};
use drivers_common::services::interrupts::{
    CpuMask, InterruptsService, IrqBinding, IrqHandler, IrqNumber, IrqPriority,
    IrqRegistrationError,
};

pub struct InterruptsControlAdapter {
    service: Arc<dyn InterruptsService>,
}

impl InterruptsControlAdapter {
    pub fn new(service: Arc<dyn InterruptsService>) -> Self {
        Self { service }
    }
}

/// Мост drivers-common `IrqHandler` -> capability `IrqSink`. Исполняется в
/// IRQ-контексте; `sink.fire()` синхронно маскирует линию и поднимает `SIGNALED`.
struct SinkHandler {
    sink: Arc<dyn IrqSink>,
}

impl IrqHandler for SinkHandler {
    fn handle(&self) {
        self.sink.fire();
    }
}

impl InterruptsControl for InterruptsControlAdapter {
    fn bind_line(&self, irq: u16, sink: Arc<dyn IrqSink>) -> Result<IrqBindToken, IpcError> {
        let binding = IrqBinding::new(
            IrqNumber::new(irq),
            // Триггер из FDT недоступен на mint-пути; линия наследует boot-ICFGR.
            None,
            IrqPriority::HIGHEST,
            CpuMask::ALL,
            Box::new(SinkHandler { sink }),
        );
        let bound = self.service.bind(binding).map_err(map_bind_error)?;
        // IrqBound (FnOnce-cleanup, не Clone) переезжает во владение токена;
        // его Drop отключит линию и удалит обработчик.
        Ok(IrqBindToken::new(bound))
    }

    fn mask(&self, irq: u16) {
        self.service.mask(IrqNumber::new(irq));
    }

    fn unmask(&self, irq: u16) {
        self.service.unmask(IrqNumber::new(irq));
    }
}

fn map_bind_error(err: IrqRegistrationError) -> IpcError {
    match err {
        // Линия уже занята — аналог исчерпания ресурса.
        IrqRegistrationError::AlreadyRegistered => IpcError::ResourceExhausted,
        IrqRegistrationError::InvalidIrq
        | IrqRegistrationError::Unsupported
        | IrqRegistrationError::Other(_) => IpcError::WrongType,
    }
}
