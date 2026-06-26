extern crate alloc;

use alloc::sync::Arc;
use core::fmt::{Display, Formatter};

use scheduler::SchedulerService;

use crate::services::{
    console::ConsoleService, interrupts::InterruptsService, mmio::MmioService, timer::TimerService,
};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ServiceKind {
    Mmio,
    Console,
    Interrupts,
    Timer,
    Scheduler,
}

impl ServiceKind {
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Mmio => "MmioService",
            Self::Console => "ConsoleService",
            Self::Interrupts => "InterruptsService",
            Self::Timer => "TimerService",
            Self::Scheduler => "SchedulerService",
        }
    }
}

impl Display for ServiceKind {
    fn fmt(&self, f: &mut Formatter<'_>) -> core::fmt::Result {
        f.write_str(self.as_str())
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum BootServicesError {
    Missing(ServiceKind),
    Duplicate(ServiceKind),
}

impl Display for BootServicesError {
    fn fmt(&self, f: &mut Formatter<'_>) -> core::fmt::Result {
        match self {
            Self::Missing(service) => write!(f, "Service is missing: {service}"),
            Self::Duplicate(service) => write!(f, "Service is already registered: {service}"),
        }
    }
}

pub struct BootServices {
    mmio: Option<Arc<dyn MmioService>>,
    console: Option<Arc<dyn ConsoleService>>,
    interrupts: Option<Arc<dyn InterruptsService>>,
    timer: Option<Arc<dyn TimerService>>,
    scheduler: Option<Arc<dyn SchedulerService>>,
}

impl BootServices {
    pub const fn new() -> Self {
        Self {
            mmio: None,
            console: None,
            interrupts: None,
            timer: None,
            scheduler: None,
        }
    }

    pub fn mmio(&self) -> Option<Arc<dyn MmioService>> {
        self.mmio.clone()
    }

    pub fn require_mmio(&self) -> Result<Arc<dyn MmioService>, BootServicesError> {
        self.mmio()
            .ok_or(BootServicesError::Missing(ServiceKind::Mmio))
    }

    pub fn set_mmio(&mut self, service: Arc<dyn MmioService>) -> Result<(), BootServicesError> {
        if self.mmio.is_some() {
            return Err(BootServicesError::Duplicate(ServiceKind::Mmio));
        }
        self.mmio = Some(service);
        Ok(())
    }

    pub fn console(&self) -> Option<Arc<dyn ConsoleService>> {
        self.console.clone()
    }

    pub fn require_console(&self) -> Result<Arc<dyn ConsoleService>, BootServicesError> {
        self.console()
            .ok_or(BootServicesError::Missing(ServiceKind::Console))
    }

    pub fn set_console(
        &mut self,
        service: Arc<dyn ConsoleService>,
    ) -> Result<(), BootServicesError> {
        if self.console.is_some() {
            return Err(BootServicesError::Duplicate(ServiceKind::Console));
        }
        self.console = Some(service);
        Ok(())
    }

    pub fn interrupts(&self) -> Option<Arc<dyn InterruptsService>> {
        self.interrupts.clone()
    }

    pub fn require_interrupts(&self) -> Result<Arc<dyn InterruptsService>, BootServicesError> {
        self.interrupts()
            .ok_or(BootServicesError::Missing(ServiceKind::Interrupts))
    }

    pub fn set_interrupts(
        &mut self,
        service: Arc<dyn InterruptsService>,
    ) -> Result<(), BootServicesError> {
        if self.interrupts.is_some() {
            return Err(BootServicesError::Duplicate(ServiceKind::Interrupts));
        }
        self.interrupts = Some(service);
        Ok(())
    }

    pub fn timer(&self) -> Option<Arc<dyn TimerService>> {
        self.timer.clone()
    }

    pub fn require_timer(&self) -> Result<Arc<dyn TimerService>, BootServicesError> {
        self.timer()
            .ok_or(BootServicesError::Missing(ServiceKind::Timer))
    }

    pub fn set_timer(&mut self, service: Arc<dyn TimerService>) -> Result<(), BootServicesError> {
        if self.timer.is_some() {
            return Err(BootServicesError::Duplicate(ServiceKind::Timer));
        }
        self.timer = Some(service);
        Ok(())
    }

    pub fn scheduler(&self) -> Option<Arc<dyn SchedulerService>> {
        self.scheduler.clone()
    }

    pub fn require_scheduler(&self) -> Result<Arc<dyn SchedulerService>, BootServicesError> {
        self.scheduler()
            .ok_or(BootServicesError::Missing(ServiceKind::Scheduler))
    }

    pub fn set_scheduler(
        &mut self,
        service: Arc<dyn SchedulerService>,
    ) -> Result<(), BootServicesError> {
        if self.scheduler.is_some() {
            return Err(BootServicesError::Duplicate(ServiceKind::Scheduler));
        }
        self.scheduler = Some(service);
        Ok(())
    }
}

impl Default for BootServices {
    fn default() -> Self {
        Self::new()
    }
}

#[cfg(test)]
mod tests {
    extern crate std;

    use alloc::{boxed::Box, sync::Arc};
    use core::sync::atomic::{AtomicBool, Ordering};
    use std::sync::Mutex;

    use io::writer::Writer;
    use memory::mem_flags::{DeviceMemoryPermission, Owners};

    use super::*;
    use crate::services::{
        interrupts::{InterruptsService, IrqBinding, IrqBound, IrqNumber, IrqRegistrationError},
        mmio::{MmioAddress, MmioBound, MmioMapError},
        timer::TickHandler,
    };

    struct TestMmioService;

    impl MmioService for TestMmioService {
        fn map_mmio(
            &self,
            _address: MmioAddress,
            _permissions: Owners<DeviceMemoryPermission>,
        ) -> Result<MmioBound, MmioMapError> {
            let cleanup = Box::new(|_, _| {});
            Ok(MmioBound::new(
                MmioAddress::new(0x1000, 0x1000).unwrap(),
                memory::virtual_address::PageAlignedVirtualAddress::from_usize(0x1000).unwrap(),
                cleanup,
            ))
        }
    }

    struct TestConsole;

    impl Writer for TestConsole {
        fn write_all(&self, _buf: &[u8]) {}

        fn flush(&self) {}
    }

    struct TestInterruptsService;

    impl InterruptsService for TestInterruptsService {
        fn enable(&self) {}

        fn disable(&self) {}

        fn bind(&self, _binding: IrqBinding) -> Result<IrqBound, IrqRegistrationError> {
            Ok(IrqBound::new(|| {}))
        }

        fn mask(&self, _irq: IrqNumber) {}

        fn unmask(&self, _irq: IrqNumber) {}

        fn dispatch_interrupt(&self) {}
    }

    struct TestTimerService {
        handler_installed: AtomicBool,
    }

    impl TestTimerService {
        fn new() -> Self {
            Self {
                handler_installed: AtomicBool::new(false),
            }
        }
    }

    impl TimerService for TestTimerService {
        fn now_ns(&self) -> u64 {
            0
        }

        fn schedule_next(&self, _deadline_ns: u64) {}

        fn set_handler(&self, _handler: Arc<dyn TickHandler>) {
            self.handler_installed.store(true, Ordering::SeqCst);
        }
    }

    struct TestSchedulerService {
        spawn_calls: Mutex<usize>,
    }

    impl TestSchedulerService {
        fn new() -> Self {
            Self {
                spawn_calls: Mutex::new(0),
            }
        }
    }

    impl SchedulerService for TestSchedulerService {
        fn spawn_boxed(
            &self,
            _cfg: scheduler::SpawnConfig,
            _entry: Box<dyn FnOnce() + Send + 'static>,
        ) -> Result<scheduler::ThreadId, scheduler::SpawnError> {
            *self.spawn_calls.lock().unwrap() += 1;
            Err(scheduler::SpawnError::NoFreeThreadSlots)
        }

        fn yield_now(&self) {}

        fn sleep_ns(&self, _ns: u64) {}

        fn current(&self) -> scheduler::ThreadId {
            panic!("not used in tests")
        }

        fn exit(&self) -> ! {
            panic!("not used in tests")
        }
    }

    #[test]
    fn missing_service_reports_concrete_kind() {
        let services = BootServices::new();
        let Err(err) = services.require_timer() else {
            panic!("timer service must be missing")
        };
        assert_eq!(err, BootServicesError::Missing(ServiceKind::Timer));
    }

    #[test]
    fn register_and_resolve_all_services() {
        let mut services = BootServices::new();

        services
            .set_mmio(Arc::new(TestMmioService))
            .expect("mmio registration must succeed");
        services
            .set_console(Arc::new(TestConsole))
            .expect("console registration must succeed");
        services
            .set_interrupts(Arc::new(TestInterruptsService))
            .expect("interrupts registration must succeed");
        services
            .set_timer(Arc::new(TestTimerService::new()))
            .expect("timer registration must succeed");
        services
            .set_scheduler(Arc::new(TestSchedulerService::new()))
            .expect("scheduler registration must succeed");

        assert!(services.mmio().is_some());
        assert!(services.console().is_some());
        assert!(services.interrupts().is_some());
        assert!(services.timer().is_some());
        assert!(services.scheduler().is_some());
    }

    #[test]
    fn duplicate_registration_preserves_existing_service() {
        let mut services = BootServices::new();
        let first: Arc<dyn ConsoleService> = Arc::new(TestConsole);
        let second: Arc<dyn ConsoleService> = Arc::new(TestConsole);

        services
            .set_console(first.clone())
            .expect("first registration must succeed");
        let err = services
            .set_console(second)
            .expect_err("duplicate registration must fail");

        assert_eq!(err, BootServicesError::Duplicate(ServiceKind::Console));
        let resolved = services
            .require_console()
            .expect("original console must stay installed");
        assert!(Arc::ptr_eq(&first, &resolved));
    }
}
