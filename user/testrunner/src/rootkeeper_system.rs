//! Композиция rootkeeper по плану загрузки: чтение плана из образа и наблюдение
//! supervisor за завершением компонента.

use alloc::vec::Vec;
use core::num::NonZeroU64;

use filesystem_abi::{
    AccessMode, FileSystemError, Mount, NamespaceClient, OpenFlags, Opened, Path,
    decode_mount_table,
};
use ipc::wire::IpcError;
use kernel_tests::kernel_test;
use managed_component::ManagedFailure;
use rootkeeper::{BootFailure, RootSystem, Stopped, Supervised, SystemImage};
use runtime::{OwnedHandle, PortTransport, Signal, Timeout};
use syscall::SIGNALED;
use userland_image::{
    BOOT_PLAN_PATH, ComponentSpec, PlanComponent, PlanDecodeError, decode_plan, encode_plan,
};

use crate::filesystem::{CALL_TIMEOUT, connect, path};

pub(crate) const BOOT_PLAN_ENTRY: &str = "/boot/testrunner.plan";

// Код выхода, которым сценарий снимает маршрутизатор: supervisor обязан
// увидеть именно его.
const ROUTER_EXIT_CODE: u32 = 73;

// Значок namespace-сессии, которой сценарии ходят к дереву имён.
const PROBE_BADGE: NonZeroU64 = NonZeroU64::new(9).expect("badge is non-zero");

// Изменяемое монтирование дерева имён загрузки.
const RAM_MOUNT: &str = "/tmp";

const ROUTER_PROGRAM: &str = "/boot/namespace-router";
const ROUTER_BUDGET_PAGES: u64 = 2_048;
const ROUTER_STARTUP_TIMEOUT: Timeout = Timeout::from_secs(2);

const BOOT_MOUNTS: &str = "/boot/namespace.mounts";
const CRASH_MOUNTS: &str = "/boot/crash.mounts";
const BROKEN_MOUNTS: &str = "/boot/broken.mounts";
const NO_MOUNTS: &str = "/boot/empty.mounts";

const BOOT_TREE: &[Mount<'static>] = &[
    Mount {
        path: "/",
        program: "/boot/filesystem-imagefs",
    },
    Mount {
        path: RAM_MOUNT,
        program: "/boot/filesystem-ramfs",
    },
];

const MOUNT_TABLE_EXIT_CODE: u32 = 8;

// Заходов на падающее монтирование: их число обязано превысить предел его перезапусков.
const CRASH_ATTEMPTS: usize = 8;

// Экземпляров, которые маршрутизатор поднимает одному монтированию, считая первый.
const MOUNT_INCARNATIONS: usize = 3;

// Запусков маршрутизатора, которые выполняет supervisor, считая первый.
const ROUTER_INCARNATIONS: u32 = 3;

// Заходов на маршрутизатор: их число обязано превысить предел его запусков.
const ROUTER_ATTEMPTS: usize = 6;

// Ожидание смены ушедшего экземпляра: она приходит оборотом наблюдения маршрутизатора.
const RESTART_ATTEMPTS: usize = 100;
const RESTART_TURN: Timeout = Timeout::from_ns(20_000_000);

fn plan_bytes(mounts: &str, critical: bool, restart_limit: u32) -> Vec<u8> {
    encode_plan(&ComponentSpec {
        id: "filesystem",
        program: ROUTER_PROGRAM,
        config: mounts,
        budget_pages: ROUTER_BUDGET_PAGES,
        startup_timeout: ROUTER_STARTUP_TIMEOUT,
        restart_limit,
        critical,
    })
    .expect("scenario plan encodes")
}

fn boot_plan(mounts: &str) -> Vec<u8> {
    plan_bytes(mounts, true, ROUTER_INCARNATIONS)
}

fn image() -> SystemImage {
    SystemImage::acquire(crate::bootstrap_handle()).expect("userland image is mapped")
}

fn component(bytes: &[u8]) -> PlanComponent<'_> {
    decode_plan(bytes)
        .expect("scenario plan decodes")
        .component()
}

fn try_session(system: &RootSystem<'_>, prefix: &str) -> Result<OwnedHandle, FileSystemError> {
    let namespace = system
        .managed()
        .running()
        .expect("namespace router is running")
        .roots()
        .session(PROBE_BADGE)
        .expect("namespace session minted");
    // Отказ транспорта на пути к ещё не обслуживающему порт маршрутизатору неотличим
    // для клиента от неотвечающего монтирования.
    let mounted = NamespaceClient::new(PortTransport::client(&namespace))
        .with_wait(CALL_TIMEOUT)
        .resolve(path(prefix))
        .map_err(|_| FileSystemError::Unresponsive)??;
    Ok(OwnedHandle::adopt(mounted.session))
}

// Ждёт сессию, пока маршрутизатор поднимает монтированию смену: между экземплярами путь накрыт,
// но сессий не выдаёт.
fn await_session(system: &RootSystem<'_>, prefix: &str) -> Result<OwnedHandle, FileSystemError> {
    for _ in 0..RESTART_ATTEMPTS {
        match try_session(system, prefix) {
            Err(FileSystemError::Unresponsive) => pause(RESTART_TURN),
            outcome => return outcome,
        }
    }
    Err(FileSystemError::Unresponsive)
}

fn await_tree(system: &RootSystem<'_>, prefix: &str) -> OwnedHandle {
    // Дерево имён поднимает сам маршрутизатор, и стартовый кадр только доставлен: путь
    // становится накрытым его оборотами.
    for _ in 0..RESTART_ATTEMPTS {
        match try_session(system, prefix) {
            Ok(session) => return session,
            Err(_) => pause(RESTART_TURN),
        }
    }
    panic!("boot tree is not mounted")
}

// Вызов, уносящий процесс монтирования: фикстура падает на переносе имени, и ответить на вызов
// уже некому.
fn crash(session: &OwnedHandle) {
    let root = path(Path::ROOT.as_str());
    let outcome = connect(session).rename(root, root);
    kernel_tests::kassert_eq!(outcome, Err(IpcError::PeerClosed));
}

fn router_alive(system: &RootSystem<'_>) -> bool {
    system
        .managed()
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

fn terminate_router(system: &RootSystem<'_>) -> bool {
    let terminated = system
        .managed()
        .running()
        .map(|running| running.process().terminate(ROUTER_EXIT_CODE));
    matches!(terminated, Some(Ok(())))
}

#[kernel_test]
fn compiled_plan_carries_the_boot_composition() {
    let image = image();

    let plan = image.plan(BOOT_PLAN_ENTRY).expect("compiled plan decodes");

    let component = plan.component();
    kernel_tests::kassert_eq!(component.program(), ROUTER_PROGRAM);
    kernel_tests::kassert_eq!(component.budget_pages(), ROUTER_BUDGET_PAGES);
    kernel_tests::kassert_eq!(component.startup_timeout(), ROUTER_STARTUP_TIMEOUT);
    kernel_tests::kassert_eq!(component.restart_limit(), ROUTER_INCARNATIONS);
    kernel_tests::kassert!(component.critical());

    kernel_tests::kassert_eq!(component.config(), BOOT_MOUNTS);
}

#[kernel_test]
fn compiled_mount_table_carries_the_boot_tree() {
    let image = image();
    let entries = image.entries().expect("image decodes");

    let record = entries
        .entry(BOOT_MOUNTS)
        .expect("image carries the compiled mount table");
    let table = decode_mount_table(record.data()).expect("compiled table decodes");

    kernel_tests::kassert_eq!(table.len(), BOOT_TREE.len());
    kernel_tests::kassert_eq!(table.get(0), Some(BOOT_TREE[0]));
    kernel_tests::kassert_eq!(table.get(1), Some(BOOT_TREE[1]));
}

#[kernel_test]
fn boot_without_a_plan_record_names_its_failure() {
    let image = image();

    // Образ прогона несёт план по своему пути: записи загрузочного плана в нём нет.
    let missing = image.plan(BOOT_PLAN_PATH).err();
    kernel_tests::kassert_eq!(missing, Some(BootFailure::PlanMissing));
    kernel_tests::kassert_eq!(missing.map(BootFailure::stopped), Some(Stopped::BadImage));
}

#[kernel_test]
fn boot_with_a_corrupted_plan_record_names_its_failure() {
    let image = image();

    // Запись программы планом не является: её байты декодер отвергает.
    let corrupted = image.plan(ROUTER_PROGRAM).err();
    kernel_tests::kassert!(matches!(
        corrupted,
        Some(BootFailure::Plan(PlanDecodeError::InvalidMagic(_)))
    ));
}

#[kernel_test]
fn a_mount_table_that_does_not_come_up_stops_the_composition() {
    let log = crate::log_relay::router_log_session();
    let relay_thread = crate::log_relay::shared_relay_thread().expect("shared relay is running");
    let image = image();
    let bytes = plan_bytes(BROKEN_MOUNTS, true, 1);
    let mut system =
        RootSystem::bring_up(&image, component(&bytes), log).expect("router startup delivered");

    let supervised = system.supervise_turn(crate::bootstrap_handle(), relay_thread);

    // Дерево имён поднимает маршрутизатор, и отказ монтирования уносит его процесс вместе с
    // уже поднятыми монтированиями: наружу дерево уходит либо целиком, либо никак.
    kernel_tests::kassert_eq!(supervised, Supervised::Stopped(Stopped::Subsystem));
    kernel_tests::kassert_eq!(
        system.managed().failure(),
        Some(ManagedFailure::Exit(MOUNT_TABLE_EXIT_CODE))
    );
}

#[kernel_test]
fn supervisor_restarts_the_router_with_a_fresh_namespace() {
    let log = crate::log_relay::router_log_session();
    let relay_thread = crate::log_relay::shared_relay_thread().expect("shared relay is running");
    let image = image();
    let bytes = boot_plan(BOOT_MOUNTS);
    let mut system =
        RootSystem::bring_up(&image, component(&bytes), log).expect("boot filesystem namespace");
    let session = await_tree(&system, RAM_MOUNT);
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
    kernel_tests::kassert_eq!(system.managed().generation(), 2);
    let fresh = await_tree(&system, RAM_MOUNT);
    kernel_tests::kassert!(matches!(probe(&fresh), Ok(Ok(_))));
    system.shutdown();
}

#[kernel_test]
fn supervisor_stops_the_composition_at_the_router_restart_limit() {
    let log = crate::log_relay::router_log_session();
    let relay_thread = crate::log_relay::shared_relay_thread().expect("shared relay is running");
    let image = image();
    let bytes = boot_plan(NO_MOUNTS);
    let mut system =
        RootSystem::bring_up(&image, component(&bytes), log).expect("boot empty namespace");

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
    kernel_tests::kassert_eq!(system.managed().generation(), ROUTER_INCARNATIONS);
    kernel_tests::kassert_eq!(
        system.managed().failure(),
        Some(ManagedFailure::Exit(ROUTER_EXIT_CODE))
    );
}

#[kernel_test]
fn critical_component_without_restarts_stops_the_composition() {
    let log = crate::log_relay::router_log_session();
    let relay_thread = crate::log_relay::shared_relay_thread().expect("shared relay is running");
    let image = image();
    let bytes = plan_bytes(NO_MOUNTS, true, 1);
    let mut system =
        RootSystem::bring_up(&image, component(&bytes), log).expect("boot empty namespace");
    kernel_tests::kassert!(terminate_router(&system));

    let supervised = system.supervise_turn(crate::bootstrap_handle(), relay_thread);

    // Предел в один запуск исчерпан первым же отказом, и критический компонент
    // доводит композицию до остановки.
    kernel_tests::kassert_eq!(supervised, Supervised::Stopped(Stopped::Subsystem));
    kernel_tests::kassert_eq!(system.managed().generation(), 1);
}

#[kernel_test]
fn optional_component_without_restarts_leaves_the_composition_up() {
    let log = crate::log_relay::router_log_session();
    let relay_thread = crate::log_relay::shared_relay_thread().expect("shared relay is running");
    let image = image();
    let bytes = plan_bytes(NO_MOUNTS, false, 1);
    let mut system =
        RootSystem::bring_up(&image, component(&bytes), log).expect("boot empty namespace");
    kernel_tests::kassert!(terminate_router(&system));

    let supervised = system.supervise_turn(crate::bootstrap_handle(), relay_thread);

    // Некритический компонент снимается с композиции, а сама композиция остаётся
    // поднятой: её поток пересылки жив и продолжает выдавать сессии.
    kernel_tests::kassert_eq!(
        supervised,
        Supervised::Abandoned {
            exit_code: Some(ROUTER_EXIT_CODE)
        }
    );
    kernel_tests::kassert!(system.managed().running().is_none());
    drop(crate::log_relay::router_log_session());
    system.shutdown();
}

#[kernel_test]
fn router_exit_takes_down_its_mount_processes() {
    let log = crate::log_relay::router_log_session();
    let relay_thread = crate::log_relay::shared_relay_thread().expect("shared relay is running");
    let image = image();
    let bytes = boot_plan(BOOT_MOUNTS);
    let mut system =
        RootSystem::bring_up(&image, component(&bytes), log).expect("boot filesystem namespace");
    let session = await_tree(&system, RAM_MOUNT);
    kernel_tests::kassert!(matches!(probe(&session), Ok(Ok(_))));

    kernel_tests::kassert!(terminate_router(&system));
    system.supervise_turn(crate::bootstrap_handle(), relay_thread);

    // Монтирование ушло вместе с поднявшим его маршрутизатором, и его сессия
    // терминальна: сирот подсистема не оставляет.
    kernel_tests::kassert_eq!(probe(&session), Err(IpcError::PeerClosed));
}

#[kernel_test]
fn router_replaces_a_faulted_mount() {
    let log = crate::log_relay::router_log_session();
    let image = image();
    let bytes = boot_plan(CRASH_MOUNTS);
    let system =
        RootSystem::bring_up(&image, component(&bytes), log).expect("boot filesystem namespace");
    let first = await_tree(&system, Path::ROOT.as_str());
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
    let image = image();
    let bytes = boot_plan(CRASH_MOUNTS);
    let system =
        RootSystem::bring_up(&image, component(&bytes), log).expect("boot filesystem namespace");

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
