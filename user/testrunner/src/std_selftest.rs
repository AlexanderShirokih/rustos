//! Прогон стенда стандартной библиотеки.
//!
//! Программа на `std` поднимается управляемым компонентом, как её поднимает композиция:
//! своим стартовым кадром и своей лог-сессией. Итог её проверок - код выхода, а подробности
//! провала уходят в журнал сообщением паники.

use kernel_objects::Resource;
use kernel_tests::kernel_test;
use managed_component::{
    ComponentRoots, ComponentSlots, ManagedComponent, ManagedStatus, launch_component,
};
use runtime::Timeout;

use crate::{
    log_relay::SELFTEST_BADGE,
    managed_startup::{image_mapping, image_program, process_pages},
};

const SELFTEST_ENTRY: &str = "/boot/std-selftest/program";

const INSTANCE_NAME: &str = "std-selftest";

const SELFTEST_EXPORTS: usize = 0;

const DELIVERY_TIMEOUT: Timeout = Timeout::from_secs(1);

const PROCESS_TIMEOUT: Timeout = Timeout::from_secs(10);

const HEAP_SLACK_PAGES: u64 = 1024;

#[kernel_test]
fn std_selftest_program_passes_its_checks() {
    let mapping = image_mapping();
    let program = image_program(&mapping, SELFTEST_ENTRY);
    let parent = Resource::self_resource().expect("self metering resource");
    let mut component = ManagedComponent::<ComponentRoots>::new(
        &parent,
        process_pages(&program) + HEAP_SLACK_PAGES,
    )
    .expect("reserve the component budget");
    let log =
        crate::log_relay::component_log_session(SELFTEST_BADGE).expect("selftest log session");

    launch_component(
        &mut component,
        &program,
        INSTANCE_NAME,
        &log,
        DELIVERY_TIMEOUT,
        SELFTEST_EXPORTS,
        ComponentSlots::default(),
    )
    .expect("startup frame delivered");

    component
        .observe_exit(PROCESS_TIMEOUT)
        .expect("observe selftest exit");
    kernel_tests::kassert_eq!(component.status(), ManagedStatus::Exited);
    kernel_tests::kassert_eq!(component.failure(), None);
}
