use alloc::sync::Arc;

use drivers_common::services::interrupts::IrqHandler;
use spin::Mutex;

/// Контроллер прерываний, отдающий обработчик очередного pending IRQ.
pub(crate) trait DispatchController {
    fn dispatch_interrupt(&mut self) -> Option<Arc<dyn IrqHandler>>;
}

/// Вспомогательная функция для обработки прерываний
pub(crate) fn dispatch_interrupt<C: DispatchController>(controller: &Mutex<C>) {
    let handler = controller.lock().dispatch_interrupt();

    if let Some(handler) = handler {
        handler.handle();
    }
}
