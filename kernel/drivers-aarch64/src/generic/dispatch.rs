use alloc::sync::Arc;

use collections::{LockCell, MutexCell};
use drivers_common::services::interrupts::IrqHandler;

/// Контроллер прерываний, отдающий обработчик очередного pending IRQ.
pub(crate) trait DispatchController {
    fn dispatch_interrupt(&mut self) -> Option<Arc<dyn IrqHandler>>;
}

/// Вспомогательная функция для обработки прерываний
pub(crate) fn dispatch_interrupt<C: DispatchController>(controller: &MutexCell<C>) {
    let handler = controller.with_lock(DispatchController::dispatch_interrupt);

    if let Some(handler) = handler {
        handler.handle();
    }
}
