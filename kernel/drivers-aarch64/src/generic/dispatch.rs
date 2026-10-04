use alloc::sync::Arc;

use drivers_common::services::interrupts::IrqHandler;
use kbase::{LockCell, MutexCell};

pub(crate) trait DispatchController {
    fn dispatch_interrupt(&mut self) -> Option<Arc<dyn IrqHandler>>;
}

pub(crate) fn dispatch_interrupt<C: DispatchController>(controller: &MutexCell<C>) {
    let handler = controller.with_lock(DispatchController::dispatch_interrupt);

    if let Some(handler) = handler {
        handler.handle();
    }
}
