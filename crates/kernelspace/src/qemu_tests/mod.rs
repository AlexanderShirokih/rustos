//! Test-mode ядра под `cfg(feature = "qemu-tests")`.
//!
//! `kmain` после bootstrap'а создаёт init-таск и под этой фичей вызывает
//! [`run`] - он подключает console writer и запускает все
//! зарегистрированные тест-кейсы через `test_harness_qemu::run_all_tests()`.
//!
//! Кейсы распределены по подмодулям и регистрируются через
//! `register_test!`. Линкер-секция `.tests.kernel` собирает их со всех
//! модулей, поэтому новый файл достаточно объявить ниже через `mod`,
//! без дополнительных правок раннера. Тесты работают в полноценном
//! окружении ядра: живой scheduler, драйверы, прерывания, аллокатор.

#![allow(unsafe_code)]

extern crate alloc;

use alloc::sync::Arc;

use drivers_common::{CapabilityStoreExt, services::console::ConsoleService};
use io::writer::Writer;
use scheduler::{ArchContext, Bootstrapped, Scheduler, SchedulerService};
use spin::Once;

use crate::{
    kernel_context::KernelContext,
    scheduler_bootstrap::KernelTimerSource,
    user_process::{SchedulerUserProcessLauncher, UserProcessLauncher},
};

mod allocator;
mod channel;
mod channel_full_api;
mod channel_via_syscall;
mod event;
mod event_via_scheduler;
mod handle_table;
mod smoke;
// E2E-тест полного пути `SchedulerService::spawn_user_process`.
mod userspace_via_scheduler;

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

/// Доступ к scheduler-сервису из test-кейсов.
pub(super) fn scheduler() -> &'static Arc<dyn SchedulerService> {
    SCHEDULER
        .get()
        .expect("SchedulerService must be cached in qemu_tests::run")
}

pub(super) fn user_process_launcher() -> &'static Arc<dyn UserProcessLauncher> {
    USER_PROCESS_LAUNCHER
        .get()
        .expect("UserProcessLauncher must be cached in qemu_tests::spawn_qemu_tests_process")
}

/// Init-таск под `feature = "qemu-tests"`: спавнит worker-процесс
/// с приоритетом `highest`, который вызывает [`run`] и завершает QEMU
/// через ARM semihosting.
pub fn spawn_qemu_tests_process<A>(
    scheduler: &Scheduler<A, KernelTimerSource, Bootstrapped>,
    kernel: &mut KernelContext,
) where
    A: ArchContext,
{
    /// `*mut KernelContext` не Send автоматически; обёртка делает его
    /// перемещаемым в spawn-closure. Безопасность гарантируется тем, что
    /// `kmain` после spawn-а уходит в `scheduler.start()` и не
    /// обращается к контексту, а тестовый таск завершает QEMU через
    /// semihosting и тоже не возвращается.
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
            scheduler::SpawnConfig::new("qemu-tests").priority(scheduler::Priority::highest()),
            move || {
                let captured = kernel_ptr;
                // SAFETY: см. комментарий выше.
                let kernel = unsafe { &mut *captured.0 };
                run(kernel)
            },
        )
        .expect("qemu-tests process spawn must succeed");
}

/// Подключает harness к ядру и запускает все зарегистрированные кейсы.
/// Не возвращается: завершает QEMU через ARM semihosting.
pub fn run(kernel: &mut KernelContext) -> ! {
    kernel.with_runtime_state(|caps, _| {
        let console = caps
            .require_service::<dyn ConsoleService>()
            .expect("ConsoleService must be available for qemu-tests");
        ADAPTER.call_once(|| ConsoleAdapter(console));

        let scheduler = caps
            .require_service::<dyn SchedulerService>()
            .expect("SchedulerService must be available for qemu-tests");
        SCHEDULER.call_once(|| scheduler);
    });

    let writer: &'static (dyn Writer + Send + Sync) =
        ADAPTER.get().expect("ADAPTER initialised above");
    test_harness_qemu::runner::install_writer(writer);
    test_harness_qemu::run_all_tests()
}
