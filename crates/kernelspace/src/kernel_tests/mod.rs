//! Test-mode ядра под `cfg(feature = "kernel-tests")`.
//!
//! `kmain` после bootstrap'а создаёт init-таск и под этой фичей вызывает
//! [`run`] - он подключает console writer и запускает все
//! зарегистрированные тест-кейсы через `kernel_tests::run_all_tests()`.
//!
//! Кейсы распределены по подмодулям и обычно помечаются атрибутом
//! `#[kernel_test]`. Линкер-секция `.tests.kernel` собирает их со всех
//! модулей, поэтому новый файл достаточно объявить ниже через `mod`:
//! раннер подхватит новый тест автоматически. Тесты работают в
//! полноценном окружении ядра: живой scheduler, драйверы, прерывания,
//! аллокатор.

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

/// Доступ к scheduler-сервису из test-кейсов.
pub fn scheduler() -> &'static Arc<dyn SchedulerService> {
    SCHEDULER
        .get()
        .expect("SchedulerService must be cached in kernel_tests::run")
}

pub fn user_process_launcher() -> &'static Arc<dyn UserProcessLauncher> {
    USER_PROCESS_LAUNCHER
        .get()
        .expect("UserProcessLauncher must be cached in kernel_tests::spawn_kernel_tests_process")
}

/// Байты userland blob (initrd), если загрузчик их передал. `None` -
/// blob отсутствует (тест должен явно провалиться, а не молча пропуститься).
pub fn userland_blob() -> Option<&'static [u8]> {
    USERLAND_BLOB.get().copied()
}

/// Init-таск под `feature = "kernel-tests"`: спавнит worker-процесс
/// с приоритетом `highest`, который вызывает [`run`] и завершает эмулятор
/// через ARM semihosting.
pub fn spawn_kernel_tests_process<A>(
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

/// Подключает harness к ядру и запускает все зарегистрированные кейсы.
/// Не возвращается: завершает QEMU через ARM semihosting.
pub fn run(kernel: &mut KernelContext) -> ! {
    kernel.with_runtime_state(|services, _| {
        let console = services
            .require_console()
            .expect("ConsoleService must be available for kernel-tests");
        ADAPTER.call_once(|| ConsoleAdapter(console));

        let scheduler = services
            .require_scheduler()
            .expect("SchedulerService must be available for kernel-tests");
        SCHEDULER.call_once(|| scheduler);
    });

    // Кэшируем в worker-треде (а не в init-таске до scheduler.start()):
    // удержание `&'static`-среза blob-а до старта планировщика приводило к
    // зависанию неродственного user-process теста.
    if let Some(blob) = kernel.userland_blob() {
        USERLAND_BLOB.call_once(|| blob);
    }

    let writer: &'static (dyn Writer + Send + Sync) =
        ADAPTER.get().expect("ADAPTER initialised above");
    kernel_tests::runner::install_writer(writer);
    kernel_tests::run_all_tests()
}
