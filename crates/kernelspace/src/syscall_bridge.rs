//! Глобальная регистрация ядерных сервисов, нужных в местах, где
//! `KernelContext` недоступен напрямую.
//!
//! Хранит:
//! - `Arc<dyn SchedulerService>` - удобный back-channel для тестов и
//!   дополнительных kernel-точек входа без `KernelContext`.
//! - `&'static dyn AddressSpaceFactory` - back-channel для модулей,
//!   создающих user-AS без `KernelContext` на руках.

use alloc::sync::Arc;

use memory::memory_mapper::AddressSpaceFactory;
use scheduler::SchedulerService;
use spin::Once;

static SCHEDULER: Once<Arc<dyn SchedulerService>> = Once::new();

/// Глобальная фабрика user-AS. Регистрируется один раз при инициализации
/// памяти; используется scheduler-ом для создания нового адресного
/// пространства при spawn user-thread.
static ADDRESS_SPACE_FACTORY: Once<&'static (dyn AddressSpaceFactory + Send + Sync)> = Once::new();

pub fn install_scheduler(service: Arc<dyn SchedulerService>) {
    assert!(
        SCHEDULER.get().is_none(),
        "SchedulerService is already installed in syscall bridge"
    );
    let _ = SCHEDULER.call_once(|| service);
}

pub fn scheduler() -> &'static Arc<dyn SchedulerService> {
    SCHEDULER
        .get()
        .expect("SchedulerService must be installed via syscall_bridge::install_scheduler")
}

/// Регистрирует глобальную `AddressSpaceFactory`. Используется как back-channel
/// для модулей, не получающих фабрику через `KernelContext` напрямую.
pub fn install_address_space_factory(factory: &'static (dyn AddressSpaceFactory + Send + Sync)) {
    assert!(
        ADDRESS_SPACE_FACTORY.get().is_none(),
        "AddressSpaceFactory is already installed in syscall bridge"
    );
    let _ = ADDRESS_SPACE_FACTORY.call_once(|| factory);
}

/// Доступ к глобальной `AddressSpaceFactory`. Возвращает `None`, если фабрика
/// не зарегистрирована.
pub fn address_space_factory() -> Option<&'static (dyn AddressSpaceFactory + Send + Sync)> {
    ADDRESS_SPACE_FACTORY.get().copied()
}
