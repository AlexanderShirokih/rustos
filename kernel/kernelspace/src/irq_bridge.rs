//! Мост между архитектурным IRQ-path и runtime-сервисом прерываний.

use alloc::sync::Arc;

use drivers_common::services::interrupts::{CpuInterfaceError, InterruptsService};
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
        assert!(
            self.service.get().is_none(),
            "InterruptsService is already installed in IRQ bridge"
        );

        let _ = self.service.call_once(|| service);
    }

    /// Настраивает per-CPU часть контроллера на текущем CPU.
    pub fn init_this_cpu(&self) -> Result<(), CpuInterfaceError> {
        let Some(service) = self.service.get() else {
            warn!("IRQ bridge is not initialized: InterruptsService is missing");
            return Err(CpuInterfaceError::NotFound);
        };

        service.init_this_cpu()
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

/// Настраивает per-CPU часть контроллера прерываний на текущем CPU.
pub fn init_interrupts_on_this_cpu() -> Result<(), CpuInterfaceError> {
    IRQ_BRIDGE.init_this_cpu()
}

#[cfg(test)]
mod tests {
    use core::sync::atomic::{AtomicUsize, Ordering};

    use drivers_common::services::interrupts::{
        IrqBinding, IrqBound, IrqNumber, IrqRegistrationError,
    };

    use super::*;

    struct SpyInterruptsService {
        dispatch_calls: AtomicUsize,
        cpu_init_calls: AtomicUsize,
    }

    impl SpyInterruptsService {
        fn new() -> Self {
            Self {
                dispatch_calls: AtomicUsize::new(0),
                cpu_init_calls: AtomicUsize::new(0),
            }
        }

        fn dispatch_calls(&self) -> usize {
            self.dispatch_calls.load(Ordering::SeqCst)
        }

        fn cpu_init_calls(&self) -> usize {
            self.cpu_init_calls.load(Ordering::SeqCst)
        }
    }

    impl InterruptsService for SpyInterruptsService {
        fn enable(&self) {}

        fn disable(&self) {}

        fn bind(&self, _binding: IrqBinding) -> Result<IrqBound, IrqRegistrationError> {
            Err(IrqRegistrationError::Unsupported)
        }

        fn mask(&self, _irq: IrqNumber) {}

        fn unmask(&self, _irq: IrqNumber) {}

        fn init_this_cpu(&self) -> Result<(), CpuInterfaceError> {
            self.cpu_init_calls.fetch_add(1, Ordering::SeqCst);
            Ok(())
        }

        fn dispatch_interrupt(&self) {
            self.dispatch_calls.fetch_add(1, Ordering::SeqCst);
        }
    }

    #[test]
    fn dispatch_without_install_is_noop() {
        let bridge = IrqBridge::new();
        bridge.dispatch();

        let service = Arc::new(SpyInterruptsService::new());
        bridge.install(service.clone());
        assert_eq!(
            service.dispatch_calls(),
            0,
            "no-op dispatch must not have invoked any service"
        );
        bridge.dispatch();
        assert_eq!(service.dispatch_calls(), 1);
    }

    #[test]
    fn cpu_init_without_install_reports_failure() {
        let bridge = IrqBridge::new();
        assert_eq!(bridge.init_this_cpu(), Err(CpuInterfaceError::NotFound));

        let service = Arc::new(SpyInterruptsService::new());
        bridge.install(service.clone());
        assert_eq!(service.cpu_init_calls(), 0);

        assert_eq!(bridge.init_this_cpu(), Ok(()));
        assert_eq!(service.cpu_init_calls(), 1);
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
