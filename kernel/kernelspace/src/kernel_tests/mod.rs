//! Test-mode ядра под `cfg(feature = "kernel-tests")`.
//!
//! `kmain` после bootstrap создаёт начальный процесс и под этой фичей запускает все
//! зарегистрированные тест-кейсы через `kernel_tests::run_all_tests()`.
//!
//! Тестам вендится только консоль для маркеров.

#![allow(unsafe_code)]

extern crate alloc;

use alloc::sync::Arc;

use drivers_common::services::console::ConsoleService;
use io::writer::Writer;
use scheduler::{ArchContext, Bootstrapped, Scheduler};
use spin::Once;

use crate::{kernel_context::KernelContext, scheduler_bootstrap::KernelTimerSource};

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

/// Начальный процесс под `feature = "kernel-tests"`: спавнит worker-процесс
/// с приоритетом `highest`, который вызывает [`run`] и выключает машину
/// через [`crate::power::system_off`].
pub fn spawn_kernel_tests_process<A>(
    scheduler: &Scheduler<A, KernelTimerSource, Bootstrapped>,
    kernel: &mut KernelContext,
) -> Result<(), scheduler::SpawnError>
where
    A: ArchContext,
{
    struct KernelCtxPtr(*mut KernelContext);

    // SAFETY: `*mut KernelContext` не Send автоматически, и обёртка переносит
    // его в spawn-closure. Единственный читатель указателя - тело таска.
    unsafe impl Send for KernelCtxPtr {}

    let kernel_ptr = KernelCtxPtr(core::ptr::from_mut(kernel));

    scheduler
        .spawn(
            scheduler::SpawnConfig::new("kernel-tests")
                .priority(scheduler::Priority::highest())
                // Тест раскладки памяти занимает до 20 КБ стека.
                .stack_pages(8),
            move || {
                let captured = kernel_ptr;
                // SAFETY: см. комментарий выше.
                let kernel = unsafe { &mut *captured.0 };
                run(kernel)
            },
        )
        .map(|_| ())
}

fn harness_exit(code: u32) -> ! {
    crate::power::system_off(i32::try_from(code).unwrap_or(i32::MAX))
}

/// Подключает harness к ядру и запускает все зарегистрированные кейсы.
#[allow(clippy::expect_used)]
pub fn run(kernel: &mut KernelContext) -> ! {
    // Без консоли и её адаптера сообщить о результате прогона нечем.
    kernel.with_runtime_state(|services, _| {
        let console = services
            .require_console()
            .expect("ConsoleService must be available for kernel-tests");
        ADAPTER.call_once(|| ConsoleAdapter(console));
    });

    let writer: &'static (dyn Writer + Send + Sync) =
        ADAPTER.get().expect("ADAPTER initialised above");
    kernel_tests::runner::install_writer(writer);
    kernel_tests::runner::install_exit(harness_exit);
    kernel_tests::run_all_tests()
}
