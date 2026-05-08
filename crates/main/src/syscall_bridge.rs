//! Регистрация kernel-сервисов, нужных syscall-handler'ам и qemu-тестам.
//!
//! Хранит:
//! - `Arc<dyn SchedulerService>` - нужен trap-handler-ам (`thread_exit`),
//!   у них нет доступа к `KernelContext`.
//! - `&'static dyn AddressSpaceFactory` - нужен hal-aarch64 qemu-тестам,
//!   которые создают user-AS, не имея доступа к `KernelContext`.
//!   Production-путь (`bootstrap_scheduler`) читает фабрику напрямую
//!   через `KernelContext::address_space_factory()`.

use alloc::sync::Arc;

use drivers_common::services::scheduler::SchedulerService;
use memory::memory_mapper::AddressSpaceFactory;
use spin::Once;

static SCHEDULER: Once<Arc<dyn SchedulerService>> = Once::new();

/// Глобальная фабрика user-AS. Регистрируется один раз при инициализации
/// памяти; используется scheduler-ом для создания нового адресного
/// пространства при spawn user-thread.
static ADDRESS_SPACE_FACTORY: Once<&'static (dyn AddressSpaceFactory + Send + Sync)> = Once::new();

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
        .expect("SchedulerService must be installed via syscall_bridge::install_scheduler")
}

/// Регистрирует глобальную `AddressSpaceFactory`. Должна вызываться ровно один
/// раз (из [`KernelContext::new`]).
pub fn install_address_space_factory(factory: &'static (dyn AddressSpaceFactory + Send + Sync)) {
    assert!(
        ADDRESS_SPACE_FACTORY.get().is_none(),
        "AddressSpaceFactory is already installed in syscall bridge"
    );
    let _ = ADDRESS_SPACE_FACTORY.call_once(|| factory);
}

/// Доступ к глобальной `AddressSpaceFactory`. Возвращает `None`, если фабрика
/// не зарегистрирована (тесты с MockContext).
pub fn address_space_factory() -> Option<&'static (dyn AddressSpaceFactory + Send + Sync)> {
    ADDRESS_SPACE_FACTORY.get().copied()
}
