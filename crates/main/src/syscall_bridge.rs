//! Регистрация kernel-сервисов, нужных syscall-handler'ам.
//!
//! Хранит `Arc<dyn SchedulerService>` - единственный kernel-сервис, без
//! которого не работает `thread_exit`. Регистрация происходит один раз
//! в `kmain` после bootstrap-а scheduler-а.
//!
//! Платформенный trap-handler вызывает [`crate::syscall::dispatch`]
//! напрямую - отдельной dispatch-обёртки в этом модуле не нужно.

use alloc::sync::Arc;

use drivers_common::services::scheduler::SchedulerService;
use spin::Once;

static SCHEDULER: Once<Arc<dyn SchedulerService>> = Once::new();

/// Регистрирует `SchedulerService`. Должна вызываться ровно один раз;
/// повторный вызов - bug в порядке инициализации.
pub fn install_scheduler(service: Arc<dyn SchedulerService>) {
    assert!(
        SCHEDULER.get().is_none(),
        "SchedulerService is already installed in syscall bridge"
    );
    let _ = SCHEDULER.call_once(|| service);
}

/// Доступ к scheduler-сервису из syscall-handler'ов. `panic`, если не
/// установлен.
pub fn scheduler() -> &'static Arc<dyn SchedulerService> {
    SCHEDULER
        .get()
        .expect("SchedulerService должен быть установлен в syscall_bridge::install_scheduler")
}
