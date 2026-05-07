//! Регистрация kernel-сервисов, нужных syscall-handler'ам.
//!
//! Хранит `Arc<dyn SchedulerService>` - единственный kernel-сервис, без
//! которого не работает `thread_exit`. Регистрация происходит один раз
//! в `kmain` после bootstrap-а scheduler-а.
//!
//! Платформенный trap-handler вызывает [`crate::syscall::dispatch`]
//! напрямую - отдельной dispatch-обёртки в этом модуле не нужно.

// `unsafe impl Send/Sync` для трейт-объекта `MemoryMapper`-указателя - bridge
// между kernel-mechanism (mapper) и syscall-handler'ами на других CPU. См.
// обоснование рядом с `MemoryMapperPtr` ниже.
#![allow(unsafe_code)]

use alloc::sync::Arc;

use drivers_common::services::scheduler::SchedulerService;
use memory::memory_mapper::{AddressSpaceFactory, MemoryMapper};
use spin::Once;

static SCHEDULER: Once<Arc<dyn SchedulerService>> = Once::new();

/// Глобальный `MemoryMapper`. Регистрируется один раз в [`KernelContext::new`].
/// Нужен kernel-side тестам и будущему userspace-load пути для пометки
/// страниц UserRX/UserRW через [`MemoryMapper::remap`].
///
/// `MemoryMapper` сам по себе не требует `Send+Sync` (его реализации содержат
/// non-Sync аллокаторы фреймов внутри LockCell), но `&'static dyn MemoryMapper`
/// безопасен для shared-доступа из любого CPU: все методы трейта принимают
/// `&self` и реализация обязана сама синхронизировать доступ к page-tables.
/// Маркер-обёртка нужна только чтобы Rust разрешил поместить указатель в static.
struct MemoryMapperPtr(&'static dyn MemoryMapper);
// SAFETY: трейт-объект защищён внутренним lock'ом реализации (см. `Aarch64MemoryMapper`),
// все методы - `&self`, сами указатели - read-only после установки.
unsafe impl Sync for MemoryMapperPtr {}
// SAFETY: тот же контракт, что и для `Sync`: `&'static`-указатель, без mutable-state.
unsafe impl Send for MemoryMapperPtr {}

static MEMORY_MAPPER: Once<MemoryMapperPtr> = Once::new();

/// Глобальная фабрика user-AS. Регистрируется один раз при инициализации
/// памяти; используется scheduler-ом для создания нового адресного
/// пространства при spawn user-thread.
struct AddressSpaceFactoryPtr(&'static dyn AddressSpaceFactory);
// SAFETY: трейт уже требует `Send + Sync`; маркер нужен только чтобы
// поместить `&'static dyn`-указатель в Once.
unsafe impl Sync for AddressSpaceFactoryPtr {}
// SAFETY: см. Sync.
unsafe impl Send for AddressSpaceFactoryPtr {}

static ADDRESS_SPACE_FACTORY: Once<AddressSpaceFactoryPtr> = Once::new();

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

/// Регистрирует глобальный `MemoryMapper`. Должна вызываться ровно один раз
/// (из [`KernelContext::new`]).
pub fn install_memory_mapper(mapper: &'static dyn MemoryMapper) {
    assert!(
        MEMORY_MAPPER.get().is_none(),
        "MemoryMapper is already installed in syscall bridge"
    );
    let _ = MEMORY_MAPPER.call_once(|| MemoryMapperPtr(mapper));
}

/// Доступ к глобальному `MemoryMapper`. `panic`, если не установлен.
pub fn memory_mapper() -> &'static dyn MemoryMapper {
    MEMORY_MAPPER
        .get()
        .expect("MemoryMapper должен быть установлен в syscall_bridge::install_memory_mapper")
        .0
}

/// Регистрирует глобальную `AddressSpaceFactory`. Должна вызываться ровно один
/// раз (из [`KernelContext::new`]).
pub fn install_address_space_factory(factory: &'static dyn AddressSpaceFactory) {
    assert!(
        ADDRESS_SPACE_FACTORY.get().is_none(),
        "AddressSpaceFactory is already installed in syscall bridge"
    );
    let _ = ADDRESS_SPACE_FACTORY.call_once(|| AddressSpaceFactoryPtr(factory));
}

/// Доступ к глобальной `AddressSpaceFactory`. Возвращает `None`, если фабрика
/// не зарегистрирована (тесты с MockContext).
pub fn address_space_factory() -> Option<&'static dyn AddressSpaceFactory> {
    ADDRESS_SPACE_FACTORY.get().map(|p| p.0)
}
