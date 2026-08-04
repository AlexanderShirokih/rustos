//! Композиция rootkeeper поверх живого маршрутизатора имён: транзакционность
//! поднятия дерева имён и наблюдение supervisor за завершением подсистемы.

use core::num::NonZeroU64;

use filesystem_abi::{AccessMode, FileSystemError, NamespaceClient, OpenFlags, Opened, Path};
use ipc::wire::IpcError;
use kernel_tests::kernel_test;
use rootkeeper::{
    BOOT_MOUNTS, BootFailure, ManagedFailure, Mount, MountFailure, NamespaceBootError,
    NamespaceMountError, RootSystem, Stopped, Supervised,
};
use runtime::{OwnedHandle, PortTransport, Signal, Timeout};
use syscall::SIGNALED;

use crate::filesystem::{CALL_TIMEOUT, connect, path};

// Код выхода, которым сценарий снимает маршрутизатор: supervisor обязан
// увидеть именно его.
const ROUTER_EXIT_CODE: u32 = 73;

// Значок namespace-сессии, которой сценарии ходят к дереву имён.
const PROBE_BADGE: NonZeroU64 = NonZeroU64::new(9).expect("badge is non-zero");

// Изменяемое монтирование дерева имён загрузки.
const RAM_MOUNT: &str = "/tmp";

// Дерево имён сценариев супервизии: единственное монтирование, падающее по требованию клиента.
const CRASH_MOUNTS: &[Mount<'static>] = &[Mount {
    path: "/",
    program: "/boot/crash-filesystem",
}];

// Заходов на падающее монтирование: их число обязано превысить предел его перезапусков.
const CRASH_ATTEMPTS: usize = 8;

// Экземпляров, которые маршрутизатор поднимает одному монтированию, считая первый.
const MOUNT_INCARNATIONS: usize = 3;

// Запусков маршрутизатора, которые выполняет supervisor, считая первый.
const ROUTER_INCARNATIONS: u32 = 3;

// Пустое дерево имён: сценарию предела перезапусков монтирования не нужны.
const NO_MOUNTS: &[Mount<'static>] = &[];

// Заходов на маршрутизатор: их число обязано превысить предел его запусков.
const ROUTER_ATTEMPTS: usize = 6;

// Ожидание смены ушедшего экземпляра: она приходит оборотом наблюдения маршрутизатора.
const RESTART_ATTEMPTS: usize = 100;
const RESTART_TURN: Timeout = Timeout::from_ns(20_000_000);

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

// Сессия монтирования, накрывающего `prefix`: её выдаёт сама файловая система, поэтому она
// живёт ровно столько, сколько её процесс.
fn mount_session(system: &RootSystem, prefix: &str) -> OwnedHandle {
    try_session(system, prefix).expect("prefix is covered by a live mount")
}

fn try_session(system: &RootSystem, prefix: &str) -> Result<OwnedHandle, FileSystemError> {
    let namespace = system
        .namespace()
        .running()
        .expect("namespace router is running")
        .roots()
        .session(PROBE_BADGE)
        .expect("namespace session minted");
    let mounted = NamespaceClient::new(PortTransport::client(&namespace))
        .with_wait(CALL_TIMEOUT)
        .resolve(path(prefix))
        .expect("resolve call")?;
    Ok(OwnedHandle::adopt(mounted.session))
}

// Ждёт сессию, пока маршрутизатор поднимает монтированию смену: между экземплярами путь накрыт,
// но сессий не выдаёт.
fn await_session(system: &RootSystem, prefix: &str) -> Result<OwnedHandle, FileSystemError> {
    for _ in 0..RESTART_ATTEMPTS {
        match try_session(system, prefix) {
            Err(FileSystemError::Unresponsive) => pause(RESTART_TURN),
            outcome => return outcome,
        }
    }
    Err(FileSystemError::Unresponsive)
}

// Вызов, уносящий процесс монтирования: фикстура падает на переносе имени, и ответить на вызов
// уже некому.
fn crash(session: &OwnedHandle) {
    let root = path(Path::ROOT.as_str());
    let outcome = connect(session).rename(root, root);
    kernel_tests::kassert_eq!(outcome, Err(IpcError::PeerClosed));
}

fn router_alive(system: &RootSystem) -> bool {
    system
        .namespace()
        .running()
        .is_some_and(|running| running.process().join(Timeout::POLL).is_err())
}

// Приватный сигнал не поднимается, поэтому ожидание истекает целиком.
fn pause(timeout: Timeout) {
    let timer = Signal::create().expect("pause timer");
    let _ = timer.wait(SIGNALED, timeout);
}

// Обращение к корню монтирования: живая файловая система отвечает, ушедшая даёт терминальный
// отказ транспорта.
fn probe(session: &OwnedHandle) -> Result<Result<Opened, FileSystemError>, IpcError> {
    connect(session).open(
        path(Path::ROOT.as_str()),
        AccessMode::Read,
        OpenFlags::DIRECTORY,
    )
}

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
fn supervisor_restarts_the_router_with_a_fresh_namespace() {
    let log = crate::log_relay::router_log_session();
    let relay_thread = crate::log_relay::shared_relay_thread().expect("shared relay is running");
    let mut system = RootSystem::bring_up(crate::bootstrap_handle(), log, BOOT_MOUNTS)
        .expect("boot filesystem namespace");
    let session = mount_session(&system, RAM_MOUNT);
    kernel_tests::kassert!(terminate_router(&system));

    let supervised = system.supervise_turn(crate::bootstrap_handle(), relay_thread);

    kernel_tests::kassert_eq!(
        supervised,
        Supervised::Restarted {
            exit_code: Some(ROUTER_EXIT_CODE)
        }
    );
    // Capability ушедшей incarnation отозваны вместе с её процессами, а новая подняла дерево
    // имён заново и выдаёт свежие сессии.
    kernel_tests::kassert_eq!(probe(&session), Err(IpcError::PeerClosed));
    kernel_tests::kassert_eq!(system.namespace().generation(), 2);
    let fresh = mount_session(&system, RAM_MOUNT);
    kernel_tests::kassert!(matches!(probe(&fresh), Ok(Ok(_))));
    system.shutdown();
}

#[kernel_test]
fn supervisor_stops_the_composition_at_the_router_restart_limit() {
    let log = crate::log_relay::router_log_session();
    let relay_thread = crate::log_relay::shared_relay_thread().expect("shared relay is running");
    let mut system = RootSystem::bring_up(crate::bootstrap_handle(), log, NO_MOUNTS)
        .expect("boot empty namespace");

    let mut supervised = Supervised::Restarted { exit_code: None };
    for _ in 0..ROUTER_ATTEMPTS {
        kernel_tests::kassert!(terminate_router(&system));
        supervised = system.supervise_turn(crate::bootstrap_handle(), relay_thread);
        if matches!(supervised, Supervised::Stopped(_)) {
            break;
        }
    }

    // Исчерпав предел запусков, композиция останавливается и сохраняет код выхода последней
    // incarnation.
    kernel_tests::kassert_eq!(supervised, Supervised::Stopped(Stopped::Subsystem));
    // Остановил композицию именно предел запусков: рестарт, отказавший раньше, оставил бы
    // меньшее число попыток.
    kernel_tests::kassert_eq!(system.namespace().generation(), ROUTER_INCARNATIONS);
    kernel_tests::kassert_eq!(
        system.namespace().failure(),
        Some(ManagedFailure::Exit(ROUTER_EXIT_CODE))
    );
}

fn terminate_router(system: &RootSystem) -> bool {
    let terminated = system
        .namespace()
        .running()
        .map(|running| running.process().terminate(ROUTER_EXIT_CODE));
    matches!(terminated, Some(Ok(())))
}

#[kernel_test]
fn router_exit_takes_down_its_mount_processes() {
    let log = crate::log_relay::router_log_session();
    let relay_thread = crate::log_relay::shared_relay_thread().expect("shared relay is running");
    let mut system = RootSystem::bring_up(crate::bootstrap_handle(), log, BOOT_MOUNTS)
        .expect("boot filesystem namespace");
    let session = mount_session(&system, RAM_MOUNT);
    kernel_tests::kassert!(matches!(probe(&session), Ok(Ok(_))));

    let terminated = system
        .namespace()
        .running()
        .map(|running| running.process().terminate(ROUTER_EXIT_CODE));
    kernel_tests::kassert!(matches!(terminated, Some(Ok(()))));
    system.supervise_turn(crate::bootstrap_handle(), relay_thread);

    // Монтирование ушло вместе с поднявшим его маршрутизатором, и его сессия
    // терминальна: сирот подсистема не оставляет.
    kernel_tests::kassert_eq!(probe(&session), Err(IpcError::PeerClosed));
}

#[kernel_test]
fn router_replaces_a_faulted_mount() {
    let log = crate::log_relay::router_log_session();
    let system = RootSystem::bring_up(crate::bootstrap_handle(), log, CRASH_MOUNTS)
        .expect("boot filesystem namespace");
    let first = mount_session(&system, Path::ROOT.as_str());
    kernel_tests::kassert!(matches!(probe(&first), Ok(Ok(_))));

    crash(&first);

    // Отказ файловой системы уносит только её процесс: маршрутизатор остаётся жив и поднимает
    // монтированию новый экземпляр, а сессии ушедшего терминальны.
    kernel_tests::kassert!(router_alive(&system));
    kernel_tests::kassert_eq!(probe(&first), Err(IpcError::PeerClosed));
    let second = await_session(&system, Path::ROOT.as_str()).expect("mount got a new incarnation");
    kernel_tests::kassert!(matches!(probe(&second), Ok(Ok(_))));
}

#[kernel_test]
fn restart_limit_takes_the_mount_off_the_tree() {
    let log = crate::log_relay::router_log_session();
    let system = RootSystem::bring_up(crate::bootstrap_handle(), log, CRASH_MOUNTS)
        .expect("boot filesystem namespace");

    let mut incarnations = 0;
    let mut outcome = Ok(());
    for _ in 0..CRASH_ATTEMPTS {
        match await_session(&system, Path::ROOT.as_str()) {
            Ok(session) => {
                incarnations += 1;
                crash(&session);
            }
            Err(error) => {
                outcome = Err(error);
                break;
            }
        }
    }

    // Монтирование, исчерпавшее перезапуски, снимается с дерева имён, и накрытый им путь
    // перестаёт быть накрытым.
    kernel_tests::kassert_eq!(incarnations, MOUNT_INCARNATIONS);
    kernel_tests::kassert_eq!(outcome, Err(FileSystemError::NotFound));
    kernel_tests::kassert!(router_alive(&system));
}
