//! Композиция rootkeeper поверх живого маршрутизатора имён: транзакционность
//! поднятия дерева имён и наблюдение supervisor за завершением подсистемы.

use filesystem_abi::FileSystemError;
use kernel_tests::kernel_test;
use rootkeeper::{
    BOOT_MOUNTS, BootFailure, ManagedFailure, Mount, MountFailure, NamespaceBootError,
    NamespaceMountError, RootSystem, Stopped,
};

// Код выхода, которым сценарий снимает маршрутизатор: supervisor обязан
// увидеть именно его.
const ROUTER_EXIT_CODE: u32 = 73;

// Таблица с несуществующей программой во втором монтировании: её отказ обязан
// снять уже поднятый корень.
const BROKEN_MOUNTS: &[Mount<'static>] = &[
    Mount {
        path: "/",
        program: "/boot/filesystem-imagefs",
    },
    Mount {
        path: "/tmp",
        program: "/boot/missing-filesystem",
    },
];

#[kernel_test]
fn namespace_boot_rolls_back_a_failed_mount_table() {
    let log = crate::log_relay::router_log_session();

    let failure = RootSystem::bring_up(crate::bootstrap_handle(), log, BROKEN_MOUNTS).err();

    kernel_tests::kassert_eq!(
        failure,
        Some(BootFailure::Namespace(NamespaceBootError::Mount(
            NamespaceMountError {
                index: 1,
                failure: MountFailure::Rejected(FileSystemError::NotFound),
                rollback: None,
            }
        )))
    );
}

#[kernel_test]
fn supervisor_observes_the_router_exit() {
    let log = crate::log_relay::router_log_session();
    let relay_thread = crate::log_relay::shared_relay_thread().expect("shared relay is running");
    let mut system = RootSystem::bring_up(crate::bootstrap_handle(), log, BOOT_MOUNTS)
        .expect("boot filesystem namespace");
    let terminated = system
        .namespace()
        .running()
        .map(|running| running.process().terminate(ROUTER_EXIT_CODE));
    kernel_tests::kassert!(matches!(terminated, Some(Ok(()))));

    let stopped = system.wait_for_critical(crate::bootstrap_handle(), relay_thread);

    kernel_tests::kassert_eq!(stopped, Stopped::Subsystem);
    kernel_tests::kassert_eq!(
        system.namespace().failure(),
        Some(ManagedFailure::Exit(ROUTER_EXIT_CODE))
    );
    system.shutdown();
}
