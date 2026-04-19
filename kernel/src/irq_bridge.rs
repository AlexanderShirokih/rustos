//! Мост между архитектурным IRQ-path и runtime-сервисом прерываний.

use alloc::sync::Arc;

use drivers_common::services::interrupts::InterruptsService;
use klog::{debug, warn};
use spin::Once;

/// Мост между архитектурным IRQ-path и runtime-сервисом прерываний.
pub struct IrqBridge {
    service: Once<Arc<dyn InterruptsService>>,
}

impl IrqBridge {
    pub const fn new() -> Self {
        Self {
            service: Once::new(),
        }
    }

    pub fn install(&self, service: Arc<dyn InterruptsService>) {
        // Повторная установка означает некорректный порядок инициализации.
        if self.service.get().is_some() {
            panic!("InterruptsService is already installed in IRQ bridge");
        }

        let _ = self.service.call_once(|| service);
    }

    pub fn dispatch(&self) {
        let Some(service) = self.service.get() else {
            warn!("IRQ bridge is not initialized: InterruptsService is missing");
            return;
        };

        service.dispatch_interrupt();
    }
}

impl Default for IrqBridge {
    fn default() -> Self {
        Self::new()
    }
}

static IRQ_BRIDGE: IrqBridge = IrqBridge::new();

pub fn install_interrupts_service(service: Arc<dyn InterruptsService>) {
    debug!("Interrupts hook installed");
    IRQ_BRIDGE.install(service);
}

pub fn dispatch_interrupt() {
    IRQ_BRIDGE.dispatch();
}

#[cfg(test)]
mod tests {
    use core::sync::atomic::{AtomicUsize, Ordering};

    use drivers_common::services::interrupts::{IrqBinding, IrqBound, IrqRegistrationError};

    use super::*;

    struct SpyInterruptsService {
        dispatch_calls: AtomicUsize,
    }

    impl SpyInterruptsService {
        fn new() -> Self {
            Self {
                dispatch_calls: AtomicUsize::new(0),
            }
        }

        fn dispatch_calls(&self) -> usize {
            self.dispatch_calls.load(Ordering::SeqCst)
        }
    }

    impl InterruptsService for SpyInterruptsService {
        fn enable(&self) {}

        fn disable(&self) {}

        fn bind(&self, _binding: IrqBinding) -> Result<IrqBound, IrqRegistrationError> {
            Err(IrqRegistrationError::Unsupported)
        }

        fn dispatch_interrupt(&self) {
            self.dispatch_calls.fetch_add(1, Ordering::SeqCst);
        }
    }

    #[test]
    fn dispatch_without_install_is_noop() {
        let bridge = IrqBridge::new();
        bridge.dispatch();
    }

    #[test]
    #[should_panic(expected = "already installed")]
    fn install_twice_panics() {
        let bridge = IrqBridge::new();
        bridge.install(Arc::new(SpyInterruptsService::new()));
        bridge.install(Arc::new(SpyInterruptsService::new()));
    }

    #[test]
    fn dispatch_calls_service_once() {
        let bridge = IrqBridge::new();
        let service = Arc::new(SpyInterruptsService::new());
        bridge.install(service.clone());

        bridge.dispatch();

        assert_eq!(service.dispatch_calls(), 1);
    }
}
