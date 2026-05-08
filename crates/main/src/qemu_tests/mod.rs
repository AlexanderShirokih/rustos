//! Test-mode ядра под `cfg(feature = "qemu-tests")`.
//!
//! `kmain` после bootstrap'а создаёт init-таск и под этой фичей вызывает
//! [`run`] - он подключает console writer и запускает все
//! зарегистрированные тест-кейсы через `qemu_test_harness::run_all_tests()`.
//!
//! Кейсы распределены по подмодулям и регистрируются через
//! `register_test!`. Линкер-секция `.tests.kernel` собирает их со всех
//! модулей, поэтому новый файл достаточно объявить ниже через `mod`,
//! без дополнительных правок раннера. Тесты работают в полноценном
//! окружении ядра: живой scheduler, драйверы, прерывания, аллокатор.

extern crate alloc;

use alloc::sync::Arc;

use drivers_common::{
    CapabilityStoreExt,
    services::{console::ConsoleService, scheduler::SchedulerService},
};
use io::writer::Writer;
use spin::Once;

use crate::kernel_context::KernelContext;

mod allocator;
mod channel;
mod channel_full_api;
pub mod el0_probe;
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

/// Доступ к scheduler-сервису из test-кейсов.
pub(super) fn scheduler() -> &'static Arc<dyn SchedulerService> {
    SCHEDULER
        .get()
        .expect("SchedulerService must be cached in qemu_tests::run")
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
    qemu_test_harness::runner::install_writer(writer);
    qemu_test_harness::run_all_tests()
}
