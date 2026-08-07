//! Композиция rootkeeper по плану загрузки: чтение плана из образа, порядок
//! запуска по графу зависимостей, слоты capability и наблюдение supervisor за
//! завершением компонентов.

use alloc::vec::Vec;
use core::num::NonZeroU64;

use device_resolver::ResolveError;
use filesystem_abi::{
    AccessMode, FileSystemError, Mount, NamespaceClient, OpenFlags, Opened, Path,
    decode_mount_table,
};
use ipc::wire::IpcError;
use kernel_tests::kernel_test;
use managed_component::{ComponentRoots, ComponentSlots, ManagedComponent, ManagedFailure};
use rootkeeper::{BootFailure, RootSystem, SHUTDOWN_EXIT_CODE, Stopped, Supervised, SystemImage};
use rtc_abi::RtcClient;
use runtime::{MemoryRegion, OwnedHandle, PortTransport, Signal, Timeout};
use syscall::{Rights, SIGNALED};
use userland_image::{
    BOOT_PLAN_PATH, BootPlan, ComponentSpec, PlanDecodeError, PlanDevice, SlotSource, decode_plan,
    encode_plan,
};

use crate::filesystem::{CALL_TIMEOUT, connect, path};

pub(crate) const BOOT_PLAN_ENTRY: &str = "/boot/testrunner.plan";

const ROUTER_EXIT_CODE: u32 = 73;

const PROBE_BADGE: NonZeroU64 = NonZeroU64::new(9).expect("badge is non-zero");

const RAM_MOUNT: &str = "/tmp";

const ROUTER_PROGRAM: &str = "/boot/namespace-router";
const ROUTER_BUDGET_PAGES: u64 = 2_048;
const ROUTER_STARTUP_TIMEOUT: Timeout = Timeout::from_secs(2);

const RTC_PROGRAM: &str = "/boot/driver-pl031";
const RTC_BUDGET_PAGES: u64 = 256;
const RTC_COMPATIBLE: &str = "arm,pl031";

const CONSUMER_PROGRAM: &str = "/boot/namespace-consumer-fixture";
const CONSUMER_BUDGET_PAGES: u64 = 256;

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

const CRASH_ATTEMPTS: usize = 8;

const MOUNT_INCARNATIONS: usize = 3;

const ROUTER_INCARNATIONS: u32 = 3;

const ROUTER_ATTEMPTS: usize = 6;

const RESTART_ATTEMPTS: usize = 100;
const RESTART_TURN: Timeout = Timeout::from_ns(20_000_000);

const RTC_CALL_TIMEOUT: Timeout = Timeout::from_secs(15);

const CONSUMER_TURN: Timeout = Timeout::from_millis(200);

const DEVICE_EXIT_CODE: u32 = 41;

const IMAGE_SLOT: &[SlotSource<'static>] = &[SlotSource::Image];

pub(crate) fn image_slot(image: &MemoryRegion) -> ComponentSlots {
    let mut slots = ComponentSlots::default();
    slots
        .push(
            image
                .handle()
                .duplicate(Rights::READ | Rights::TRANSFER | Rights::DUPLICATE, None)
                .expect("image slot minted"),
        )
        .expect("slot fits the transaction");
    slots
}

fn router(mounts: &str, critical: bool, restart_limit: u32) -> ComponentSpec<'_> {
    ComponentSpec {
        id: "filesystem",
        program: ROUTER_PROGRAM,
        config: Some(mounts),
        slots: IMAGE_SLOT,
        requires: &[],
        after: &[],
        budget_pages: ROUTER_BUDGET_PAGES,
        startup_timeout: ROUTER_STARTUP_TIMEOUT,
        restart_limit,
        critical,
    }
}

fn plan_bytes(mounts: &str, critical: bool, restart_limit: u32) -> Vec<u8> {
    encode_plan(&[router(mounts, critical, restart_limit)]).expect("scenario plan encodes")
}

fn boot_plan(mounts: &str) -> Vec<u8> {
    plan_bytes(mounts, true, ROUTER_INCARNATIONS)
}

fn rtc_device(compatible: &str) -> [SlotSource<'_>; 2] {
    let device = PlanDevice {
        compatible,
        instance: 0,
        resource: 0,
    };
    [SlotSource::Irq(device), SlotSource::Mmio(device)]
}

fn rtc<'a>(slots: &'a [SlotSource<'a>]) -> ComponentSpec<'a> {
    ComponentSpec {
        id: "rtc",
        program: RTC_PROGRAM,
        config: None,
        slots,
        requires: &[],
        after: &[],
        budget_pages: RTC_BUDGET_PAGES,
        startup_timeout: ROUTER_STARTUP_TIMEOUT,
        restart_limit: 1,
        critical: false,
    }
}

fn image() -> SystemImage {
    SystemImage::acquire(crate::bootstrap_handle()).expect("userland image is mapped")
}

fn plan(bytes: &[u8]) -> BootPlan<'_> {
    decode_plan(bytes).expect("scenario plan decodes")
}

fn bring_up<'a>(image: &'a SystemImage, bytes: &'a [u8]) -> Result<RootSystem<'a>, BootFailure> {
    RootSystem::bring_up(
        image,
        plan(bytes),
        crate::bootstrap_handle(),
        crate::log_relay::component_log_session,
    )
}

fn started<'a>(image: &'a SystemImage, bytes: &'a [u8]) -> RootSystem<'a> {
    bring_up(image, bytes).expect("composition started")
}

fn managed<'a>(system: &'a RootSystem<'_>, index: u16) -> &'a ManagedComponent<ComponentRoots> {
    system
        .component(index)
        .expect("composition carries the component")
        .managed()
}

fn roots<'a>(system: &'a RootSystem<'_>, index: u16) -> &'a ComponentRoots {
    managed(system, index)
        .running()
        .expect("component is running")
        .roots()
}

fn relay_thread() -> syscall::Handle {
    crate::log_relay::shared_relay_thread().expect("shared relay is running")
}

fn try_session(system: &RootSystem<'_>, prefix: &str) -> Result<OwnedHandle, FileSystemError> {
    let namespace = roots(system, 0)
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

fn crash(session: &OwnedHandle) {
    let root = path(Path::ROOT.as_str());
    let outcome = connect(session).rename(root, root);
    kernel_tests::kassert_eq!(outcome, Err(IpcError::PeerClosed));
}

fn alive(system: &RootSystem<'_>, index: u16) -> bool {
    managed(system, index)
        .running()
        .is_some_and(|running| running.process().join(Timeout::POLL).is_err())
}

fn terminate(system: &RootSystem<'_>, index: u16, exit_code: u32) -> bool {
    let terminated = managed(system, index)
        .running()
        .map(|running| running.process().terminate(exit_code));
    matches!(terminated, Some(Ok(())))
}

fn pause(timeout: Timeout) {
    let timer = Signal::create().expect("pause timer");
    let _ = timer.wait(SIGNALED, timeout);
}

fn probe(session: &OwnedHandle) -> Result<Result<Opened, FileSystemError>, IpcError> {
    connect(session).open(
        path(Path::ROOT.as_str()),
        AccessMode::Read,
        OpenFlags::DIRECTORY,
    )
}

fn terminate_router(system: &RootSystem<'_>) -> bool {
    terminate(system, 0, ROUTER_EXIT_CODE)
}

#[kernel_test]
fn compiled_plan_carries_the_boot_composition() {
    let image = image();

    let plan = image.plan(BOOT_PLAN_ENTRY).expect("compiled plan decodes");

    kernel_tests::kassert_eq!(plan.len(), 1);
    let component = plan.component(0).expect("plan carries the router");
    kernel_tests::kassert_eq!(component.program(), ROUTER_PROGRAM);
    kernel_tests::kassert_eq!(component.budget_pages(), ROUTER_BUDGET_PAGES);
    kernel_tests::kassert_eq!(component.startup_timeout(), ROUTER_STARTUP_TIMEOUT);
    kernel_tests::kassert_eq!(component.restart_limit(), ROUTER_INCARNATIONS);
    kernel_tests::kassert!(component.critical());

    kernel_tests::kassert_eq!(component.config(), Some(BOOT_MOUNTS));
    kernel_tests::kassert_eq!(component.slots().count(), 1);
    kernel_tests::kassert_eq!(component.slots().next(), Some(SlotSource::Image));
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
    let image = image();
    let bytes = plan_bytes(BROKEN_MOUNTS, true, 1);
    let mut system = started(&image, &bytes);

    let supervised = system.supervise_turn(relay_thread());

    // Дерево имён поднимает маршрутизатор, и отказ монтирования уносит его процесс вместе с
    // уже поднятыми монтированиями: наружу дерево уходит либо целиком, либо никак.
    kernel_tests::kassert_eq!(supervised, Supervised::Stopped(Stopped::Subsystem));
    kernel_tests::kassert_eq!(
        managed(&system, 0).failure(),
        Some(ManagedFailure::Exit(MOUNT_TABLE_EXIT_CODE))
    );
}

#[kernel_test]
fn supervisor_restarts_the_router_with_a_fresh_namespace() {
    let image = image();
    let bytes = boot_plan(BOOT_MOUNTS);
    let mut system = started(&image, &bytes);
    let session = await_tree(&system, RAM_MOUNT);
    kernel_tests::kassert!(terminate_router(&system));

    let supervised = system.supervise_turn(relay_thread());

    kernel_tests::kassert_eq!(
        supervised,
        Supervised::Restarted {
            exit_code: Some(ROUTER_EXIT_CODE)
        }
    );
    // Capability ушедшей incarnation отозваны вместе с её процессами, а новая подняла дерево
    // имён заново и выдаёт свежие сессии.
    kernel_tests::kassert_eq!(probe(&session), Err(IpcError::PeerClosed));
    kernel_tests::kassert_eq!(managed(&system, 0).generation(), 2);
    let fresh = await_tree(&system, RAM_MOUNT);
    kernel_tests::kassert!(matches!(probe(&fresh), Ok(Ok(_))));
    system.shutdown();
}

#[kernel_test]
fn supervisor_stops_the_composition_at_the_router_restart_limit() {
    let image = image();
    let bytes = boot_plan(NO_MOUNTS);
    let mut system = started(&image, &bytes);

    let mut supervised = Supervised::Restarted { exit_code: None };
    for _ in 0..ROUTER_ATTEMPTS {
        kernel_tests::kassert!(terminate_router(&system));
        supervised = system.supervise_turn(relay_thread());
        if matches!(supervised, Supervised::Stopped(_)) {
            break;
        }
    }

    // Исчерпав предел запусков, композиция останавливается и сохраняет код выхода последней
    // incarnation.
    kernel_tests::kassert_eq!(supervised, Supervised::Stopped(Stopped::Subsystem));
    // Остановил композицию именно предел запусков: рестарт, отказавший раньше, оставил бы
    // меньшее число попыток.
    kernel_tests::kassert_eq!(managed(&system, 0).generation(), ROUTER_INCARNATIONS);
    kernel_tests::kassert_eq!(
        managed(&system, 0).failure(),
        Some(ManagedFailure::Exit(ROUTER_EXIT_CODE))
    );
}

#[kernel_test]
fn critical_component_without_restarts_stops_the_composition() {
    let image = image();
    let bytes = plan_bytes(NO_MOUNTS, true, 1);
    let mut system = started(&image, &bytes);
    kernel_tests::kassert!(terminate_router(&system));

    let supervised = system.supervise_turn(relay_thread());

    // Предел в один запуск исчерпан первым же отказом, и критический компонент
    // доводит композицию до остановки.
    kernel_tests::kassert_eq!(supervised, Supervised::Stopped(Stopped::Subsystem));
    kernel_tests::kassert_eq!(managed(&system, 0).generation(), 1);
}

#[kernel_test]
fn optional_component_without_restarts_leaves_the_composition_up() {
    let image = image();
    let bytes = plan_bytes(NO_MOUNTS, false, 1);
    let mut system = started(&image, &bytes);
    kernel_tests::kassert!(terminate_router(&system));

    let supervised = system.supervise_turn(relay_thread());

    // Некритический компонент снимается с композиции, а сама композиция остаётся
    // поднятой: её поток пересылки жив и продолжает выдавать сессии.
    kernel_tests::kassert_eq!(
        supervised,
        Supervised::Abandoned {
            exit_code: Some(ROUTER_EXIT_CODE)
        }
    );
    kernel_tests::kassert!(managed(&system, 0).running().is_none());
    system.shutdown();
}

#[kernel_test]
fn router_exit_takes_down_its_mount_processes() {
    let image = image();
    let bytes = boot_plan(BOOT_MOUNTS);
    let mut system = started(&image, &bytes);
    let session = await_tree(&system, RAM_MOUNT);
    kernel_tests::kassert!(matches!(probe(&session), Ok(Ok(_))));

    kernel_tests::kassert!(terminate_router(&system));
    system.supervise_turn(relay_thread());

    // Монтирование ушло вместе с поднявшим его маршрутизатором, и его сессия
    // терминальна: сирот подсистема не оставляет.
    kernel_tests::kassert_eq!(probe(&session), Err(IpcError::PeerClosed));
}

#[kernel_test]
fn router_replaces_a_faulted_mount() {
    let image = image();
    let bytes = boot_plan(CRASH_MOUNTS);
    let system = started(&image, &bytes);
    let first = await_tree(&system, Path::ROOT.as_str());
    kernel_tests::kassert!(matches!(probe(&first), Ok(Ok(_))));

    crash(&first);

    // Отказ файловой системы уносит только её процесс: маршрутизатор остаётся жив и поднимает
    // монтированию новый экземпляр, а сессии ушедшего терминальны.
    kernel_tests::kassert!(alive(&system, 0));
    kernel_tests::kassert_eq!(probe(&first), Err(IpcError::PeerClosed));
    let second = await_session(&system, Path::ROOT.as_str()).expect("mount got a new incarnation");
    kernel_tests::kassert!(matches!(probe(&second), Ok(Ok(_))));
}

#[kernel_test]
fn restart_limit_takes_the_mount_off_the_tree() {
    let image = image();
    let bytes = boot_plan(CRASH_MOUNTS);
    let system = started(&image, &bytes);

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
    kernel_tests::kassert!(alive(&system, 0));
}

#[kernel_test]
fn a_device_component_serves_time_over_a_minted_session() {
    let image = image();
    let device = rtc_device(RTC_COMPATIBLE);
    let bytes = encode_plan(&[rtc(&device)]).expect("scenario plan encodes");
    let mut system = started(&image, &bytes);

    let session = roots(&system, 0)
        .session(PROBE_BADGE)
        .expect("rtc session minted");
    let clock = RtcClient::new(PortTransport::client(&session)).with_wait(RTC_CALL_TIMEOUT);
    let now = clock.now().expect("rtc answers the time");
    // Будильник устройства снимается реальным прерыванием линии, сминченной
    // супервизором по описанию платформы.
    let ticked = clock.tick().expect("rtc alarm fires");

    kernel_tests::kassert!(ticked >= now);
    drop(session);
    // Линию устройства освобождает уход процесса драйвера: следующий минт той
    // же линии иначе получает AlreadyBound.
    system.shutdown();
}

#[kernel_test]
fn a_device_outside_the_platform_description_names_the_boot_failure() {
    let image = image();
    let device = rtc_device("acme,absent-clock");
    let bytes = encode_plan(&[rtc(&device)]).expect("scenario plan encodes");

    let failure = bring_up(&image, &bytes).err();

    kernel_tests::kassert_eq!(
        failure,
        Some(BootFailure::Device(ResolveError::DeviceNotFound))
    );
    kernel_tests::kassert_eq!(failure.map(BootFailure::stopped), Some(Stopped::Device));
}

#[kernel_test]
fn a_service_slot_carries_the_provider_session_and_stops_before_it() {
    let image = image();
    // Потребитель стоит в плане первым, а поднимается вторым: сессию слота
    // supervisor минтит из уже поднятого провайдера.
    let bytes = encode_plan(&[
        ComponentSpec {
            id: "consumer",
            program: CONSUMER_PROGRAM,
            config: None,
            slots: &[SlotSource::Service { provider: 1 }],
            requires: &[1],
            after: &[],
            budget_pages: CONSUMER_BUDGET_PAGES,
            startup_timeout: ROUTER_STARTUP_TIMEOUT,
            restart_limit: 1,
            critical: false,
        },
        router(BOOT_MOUNTS, true, 1),
    ])
    .expect("scenario plan encodes");
    let mut system = started(&image, &bytes);

    kernel_tests::kassert_eq!(system.start_order(), &[1, 0][..]);
    // Потребитель разрешает путь по сессии слота, пока провайдер жив: отказ
    // первого resolve увёл бы его собственным кодом.
    pause(CONSUMER_TURN);
    kernel_tests::kassert!(alive(&system, 0));

    system.shutdown();

    // Остановка обратна порядку запуска: потребитель снят раньше провайдера и
    // ухода его сессии увидеть не успел.
    kernel_tests::kassert_eq!(
        managed(&system, 0).failure(),
        Some(ManagedFailure::Exit(SHUTDOWN_EXIT_CODE))
    );
}

#[kernel_test]
fn an_optional_component_leaves_the_composition_and_a_critical_one_stops_it() {
    let image = image();
    let device = rtc_device(RTC_COMPATIBLE);
    // Драйвер стоит в плане первым, а поднимается вторым: порядок задаёт `after`.
    let bytes = encode_plan(&[
        ComponentSpec {
            after: &[1],
            ..rtc(&device)
        },
        router(BOOT_MOUNTS, true, 1),
    ])
    .expect("scenario plan encodes");
    let mut system = started(&image, &bytes);
    kernel_tests::kassert_eq!(system.start_order(), &[1, 0][..]);

    kernel_tests::kassert!(terminate(&system, 0, DEVICE_EXIT_CODE));
    let abandoned = system.supervise_turn(relay_thread());

    // Некритический компонент снят с композиции, а критический остался жив.
    kernel_tests::kassert_eq!(
        abandoned,
        Supervised::Abandoned {
            exit_code: Some(DEVICE_EXIT_CODE)
        }
    );
    kernel_tests::kassert!(managed(&system, 0).running().is_none());
    kernel_tests::kassert!(alive(&system, 1));

    kernel_tests::kassert!(terminate(&system, 1, ROUTER_EXIT_CODE));
    let stopped = system.supervise_turn(relay_thread());

    // Уход критического компонента останавливает всю композицию.
    kernel_tests::kassert_eq!(stopped, Supervised::Stopped(Stopped::Subsystem));
}
