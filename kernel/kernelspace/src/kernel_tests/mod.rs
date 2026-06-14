//! Test-mode ядра под `cfg(feature = "kernel-tests")`.
//!
//! `kmain` после bootstrap'а создаёт init-таск и под этой фичей запускает все
//! зарегистрированные тест-кейсы через `kernel_tests::run_all_tests()`.

#![allow(unsafe_code)]

extern crate alloc;

use alloc::sync::Arc;

use drivers_common::services::console::ConsoleService;
use io::writer::Writer;
use scheduler::{ArchContext, Bootstrapped, Scheduler, SchedulerService};
use spin::Once;

use crate::{
    kernel_context::KernelContext,
    scheduler_bootstrap::KernelTimerSource,
    user_process::{SchedulerUserProcessLauncher, UserProcessLauncher},
};

mod allocator;
mod bootstrap_log;
mod channel;
mod channel_full_api;
mod event;
mod event_syscall;
mod event_via_scheduler;
mod handle_table;
mod smoke;

struct ConsoleAdapter(Arc<dyn ConsoleService>);

impl Writer for ConsoleAdapter {
    fn write_all(&self, buf: &[u8]) {
        self.0.write_all(buf);
    }
    fn flush(&self) {
        self.0.flush();
    }
}

static ADAPTER: Once<ConsoleAdapter> = Once::new();

/// Сервис scheduler-а, кэшируемый для тестов, которые спавнят
/// дополнительные потоки (например, [`event_via_scheduler::event_signal_after_deadline`]).
static SCHEDULER: Once<Arc<dyn SchedulerService>> = Once::new();
static USER_PROCESS_LAUNCHER: Once<Arc<dyn UserProcessLauncher>> = Once::new();
static USERLAND_BLOB: Once<&'static [u8]> = Once::new();

pub fn scheduler() -> &'static Arc<dyn SchedulerService> {
    TEST_SCHEDULER
        .get()
        .expect("SchedulerService must be cached in kernel_tests::run")
}

pub fn user_process_launcher() -> &'static Arc<dyn UserProcessLauncher> {
    USER_PROCESS_LAUNCHER
        .get()
        .expect("UserProcessLauncher must be cached in kernel_tests::spawn_kernel_tests_process")
}

/// Байты userland blob (initrd), если загрузчик их передал. `None` -
/// blob отсутствует (тест должен явно провалиться).
pub fn userland_blob() -> Option<&'static [u8]> {
    USERLAND_BLOB.get().copied()
}

/// Init-таск под `feature = "kernel-tests"`: спавнит worker-процесс
/// с приоритетом `highest`, который вызывает [`run`] и выключает машину
/// через [`crate::power::system_off`].
pub fn spawn_kernel_tests_process<A>(
    scheduler: &Scheduler<A, KernelTimerSource, Bootstrapped>,
    kernel: &mut KernelContext,
) where
    A: ArchContext,
{
    /// `*mut KernelContext` не Send автоматически; обёртка делает его
    /// перемещаемым в spawn-closure.
    struct KernelCtxPtr(*mut KernelContext);

    // SAFETY: см. комментарий выше - единственный читатель указателя.
    unsafe impl Send for KernelCtxPtr {}

    USER_PROCESS_LAUNCHER.call_once(|| {
        Arc::new(SchedulerUserProcessLauncher::new(
            scheduler.handle(),
            kernel.address_space_factory(),
        ))
    });

    let kernel_ptr = KernelCtxPtr(core::ptr::from_mut(kernel));

    scheduler
        .spawn(
            scheduler::SpawnConfig::new("kernel-tests").priority(scheduler::Priority::highest()),
            move || {
                let captured = kernel_ptr;
                // SAFETY: см. комментарий выше.
                let kernel = unsafe { &mut *captured.0 };
                run(kernel)
            },
        )
        .expect("kernel-tests process spawn must succeed");
}

/// Делегат для harness-а: выключает машину через [`crate::power::system_off`].
fn harness_exit(code: u32) -> ! {
    crate::power::system_off(i32::try_from(code).unwrap_or(i32::MAX))
}

/// Подключает harness к ядру и запускает все зарегистрированные кейсы.
pub fn run(kernel: &mut KernelContext) -> ! {
    kernel.with_runtime_state(|services, _| {
        let console = services
            .require_console()
            .expect("ConsoleService must be available for kernel-tests");
        ADAPTER.call_once(|| ConsoleAdapter(console));

        let scheduler = services
            .require_scheduler()
            .expect("SchedulerService must be available for kernel-tests");
        TEST_SCHEDULER.call_once(|| scheduler);
    });

    if let Some(blob) = kernel.userland_blob() {
        USERLAND_BLOB.call_once(|| blob);
    }

    let writer: &'static (dyn Writer + Send + Sync) =
        ADAPTER.get().expect("ADAPTER initialised above");
    kernel_tests::runner::install_writer(writer);
    kernel_tests::runner::install_exit(harness_exit);
    kernel_tests::run_all_tests()
}
