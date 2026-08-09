//! Композиция rootkeeper по плану загрузки: чтение плана из образа, порядок
//! запуска по графу зависимостей, слоты capability и наблюдение supervisor за
//! завершением компонентов.

use alloc::vec::Vec;
use core::num::NonZeroU64;

use device_resolver::ResolveError;
use filesystem_abi::{
    AccessMode, FileSystemError, Mount, OpenFlags, Opened, Path, decode_mount_table, namespace,
    namespace_admin,
};
use ipc::wire::IpcError;
use kernel_tests::kernel_test;
use managed_component::{ComponentRoots, ComponentSlots, ManagedComponent, ManagedFailure};
use rootkeeper::{BootFailure, RootSystem, SHUTDOWN_EXIT_CODE, Stopped, Supervised, SystemImage};
use rtc_abi::rtc;
use runtime::{MemoryAccess, MemoryRegion, OwnedHandle, PortTransport, Signal, Timeout};
use syscall::{Rights, SIGNALED};
use userland_image::{
    BOOT_PLAN_PATH, BootPlan, ComponentSpec, DecodedImage, DeviceSpec, OnExhausted,
    PlanDecodeError, SlotSource, SlotSpec, decode_plan, encode_plan,
};

use crate::filesystem::{CALL_TIMEOUT, connect, path};

pub(crate) const BOOT_PLAN_ENTRY: &str = "/boot/testrunner/plan";

const ROUTER_EXIT_CODE: u32 = 73;

const PROBE_BADGE: NonZeroU64 = NonZeroU64::new(9).expect("badge is non-zero");

const RAM_MOUNT: &str = "/tmp";

const ROUTER_PROGRAM: &str = "/boot/namespace-router";
const ROUTER_NAME: &str = "namespace-router";

const INSTANCE_NAMES: [&str; 2] = ["names-boot", "names-spare"];
const ROUTER_MEMORY_PAGES: u64 = 2_048;
const ROUTER_STARTUP_TIMEOUT: Timeout = Timeout::from_secs(2);

const RTC_PROGRAM: &str = "/boot/driver-pl031";
const RTC_NAME: &str = "driver-pl031";
const RTC_MEMORY_PAGES: u64 = 256;
const RTC_COMPATIBLE: &str = "arm,pl031";
const RTC_TWIN_NAME: &str = "driver-pl031-twin";

const CONSUMER_PROGRAM: &str = "/boot/namespace-consumer-fixture";
const CONSUMER_NAME: &str = "namespace-consumer-fixture";
const CONSUMER_MEMORY_PAGES: u64 = 256;

const BOOT_MOUNTS_RECORD: &str = "/boot/namespace-router/mounts";

const CRASH_MOUNT: &str = "/crash";
const CRASH_FILESYSTEM: &str = "/boot/crash-filesystem";

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

const CRASH_ATTEMPTS: usize = 8;

const MOUNT_INCARNATIONS: usize = 3;

const ROUTER_INCARNATIONS: u32 = 3;

const ROUTER_ATTEMPTS: usize = 6;

const RESTART_ATTEMPTS: usize = 100;
const RESTART_TURN: Timeout = Timeout::from_ns(20_000_000);

const RTC_CALL_TIMEOUT: Timeout = Timeout::from_secs(15);

const CONSUMER_TURN: Timeout = Timeout::from_millis(200);

const DEVICE_EXIT_CODE: u32 = 41;

const ROUTER_SLOTS: &[SlotSpec<'static>] = &[
    SlotSpec::Image,
    SlotSpec::File {
        path: BOOT_MOUNTS_RECORD,
    },
];

pub(crate) fn router_slots(image: &MemoryRegion, entries: &DecodedImage<'_>) -> ComponentSlots {
    let mut slots = ComponentSlots::default();
    slots
        .push(
            image
                .handle()
                .duplicate(Rights::READ | Rights::TRANSFER | Rights::DUPLICATE, None)
                .expect("image slot minted"),
        )
        .expect("slot fits the transaction");
    let mounts = entries
        .entry(BOOT_MOUNTS_RECORD)
        .expect("image carries the boot mount table");
    let size = (mounts.data().len() as u64).div_ceil(PAGE_SIZE) * PAGE_SIZE;
    slots
        .push(
            image
                .slice(mounts.data_offset(), size, MemoryAccess::READ)
                .expect("mount table window")
                .into_handle(),
        )
        .expect("slot fits the transaction");
    slots
}

const PAGE_SIZE: u64 = syscall::PAGE_SIZE as u64;

fn router(on_exhausted: OnExhausted, launches: u32) -> ComponentSpec<'static> {
    ComponentSpec {
        executable: ROUTER_PROGRAM,
        name: ROUTER_NAME,
        slots: ROUTER_SLOTS,
        requires: &[],
        after: &[],
        memory_pages: ROUTER_MEMORY_PAGES,
        startup_timeout: ROUTER_STARTUP_TIMEOUT,
        launches,
        on_exhausted,
    }
}

fn plan_bytes(on_exhausted: OnExhausted, launches: u32) -> Vec<u8> {
    encode_plan(&[router(on_exhausted, launches)]).expect("scenario plan encodes")
}

fn boot_plan() -> Vec<u8> {
    plan_bytes(OnExhausted::Halt, ROUTER_INCARNATIONS)
}

fn rtc_device<'a>(compatible: &'a [&'a str]) -> [SlotSpec<'a>; 2] {
    let device = DeviceSpec {
        compatible,
        instance: 0,
        resource: 0,
    };
    [SlotSpec::Irq(device), SlotSpec::Mmio(device)]
}

fn rtc<'a>(slots: &'a [SlotSpec<'a>]) -> ComponentSpec<'a> {
    ComponentSpec {
        executable: RTC_PROGRAM,
        name: RTC_NAME,
        slots,
        requires: &[],
        after: &[],
        memory_pages: RTC_MEMORY_PAGES,
        startup_timeout: ROUTER_STARTUP_TIMEOUT,
        launches: 1,
        on_exhausted: OnExhausted::Detach,
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

struct Resolved {
    session: OwnedHandle,
    prefix_len: u32,
}

fn relay_thread() -> syscall::Handle {
    crate::log_relay::shared_relay_thread().expect("shared relay is running")
}

fn try_mount(system: &RootSystem<'_>, prefix: &str) -> Result<Resolved, FileSystemError> {
    let namespace = roots(system, 0)
        .session(PROBE_BADGE)
        .expect("namespace session minted");
    // Отказ транспорта на пути к ещё не обслуживающему порт маршрутизатору неотличим
    // для клиента от неотвечающего монтирования.
    let mounted = namespace::Client::new(PortTransport::client(&namespace))
        .with_wait(CALL_TIMEOUT)
        .resolve(path(prefix))
        .map_err(|_| FileSystemError::Unresponsive)??;
    Ok(Resolved {
        session: OwnedHandle::adopt(mounted.session),
        prefix_len: mounted.prefix_len,
    })
}

fn await_mount(system: &RootSystem<'_>, prefix: &str) -> Result<Resolved, FileSystemError> {
    for _ in 0..RESTART_ATTEMPTS {
        match try_mount(system, prefix) {
            Err(FileSystemError::Unresponsive) => pause(RESTART_TURN),
            outcome => return outcome,
        }
    }
    Err(FileSystemError::Unresponsive)
}

fn await_session(system: &RootSystem<'_>, prefix: &str) -> Result<OwnedHandle, FileSystemError> {
    await_mount(system, prefix).map(|resolved| resolved.session)
}

fn await_tree(system: &RootSystem<'_>, prefix: &str) -> OwnedHandle {
    // Дерево имён поднимает сам маршрутизатор, и стартовый кадр только доставлен: путь
    // становится накрытым его оборотами.
    for _ in 0..RESTART_ATTEMPTS {
        match try_mount(system, prefix) {
            Ok(resolved) => return resolved.session,
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

fn mount_crash_filesystem(system: &RootSystem<'_>) {
    // Сценарии крашей поднимают монтирование поверх дерева загрузки: своё дерево
    // маршрутизатор читает сам, и композиция его состав не выбирает.
    let admin = managed(system, 0)
        .running()
        .expect("router is running")
        .roots()
        .control();
    let mounted = namespace_admin::Client::new(PortTransport::client(admin))
        .with_wait(CALL_TIMEOUT)
        .mount(path(CRASH_MOUNT), path(CRASH_FILESYSTEM))
        .expect("mount call");
    kernel_tests::kassert!(mounted.is_ok());
}

#[kernel_test]
fn two_instances_of_one_program_run_under_their_own_names() {
    let image = image();
    let bytes = encode_plan(&[
        ComponentSpec {
            name: INSTANCE_NAMES[0],
            ..router(OnExhausted::Halt, 1)
        },
        ComponentSpec {
            name: INSTANCE_NAMES[1],
            ..router(OnExhausted::Halt, 1)
        },
    ])
    .expect("scenario plan encodes");
    let decoded = plan(&bytes);
    // Имена источников журнала выводит сам rootkeeper из записей плана: строка
    // приходит под именем экземпляра, а не под путём его программы.
    crate::log_relay::start_component_relay(decoded.components().map(|component| component.name()));

    let mut system = RootSystem::bring_up(
        &image,
        decoded,
        crate::bootstrap_handle(),
        crate::log_relay::component_scenario_session,
    )
    .expect("composition started");

    kernel_tests::kassert!(alive(&system, 0));
    kernel_tests::kassert!(alive(&system, 1));
    let sources = crate::log_relay::await_component_sources(&INSTANCE_NAMES);
    for name in INSTANCE_NAMES {
        kernel_tests::kassert!(
            sources
                .iter()
                .any(|source| source.split('/').next() == Some(name))
        );
    }
    kernel_tests::kassert!(
        !sources
            .iter()
            .any(|source| source.split('/').next() == Some(ROUTER_NAME))
    );

    system.shutdown();
    crate::log_relay::stop_component_relay();
}

#[kernel_test]
fn compiled_plan_carries_the_boot_composition() {
    let image = image();

    let plan = image.plan(BOOT_PLAN_ENTRY).expect("compiled plan decodes");

    kernel_tests::kassert_eq!(plan.len(), 1);
    let component = plan.component(0).expect("plan carries the router");
    kernel_tests::kassert_eq!(component.executable(), ROUTER_PROGRAM);
    kernel_tests::kassert_eq!(component.name(), "namespace-router");
    kernel_tests::kassert_eq!(component.memory_pages(), ROUTER_MEMORY_PAGES);
    kernel_tests::kassert_eq!(component.startup_timeout(), ROUTER_STARTUP_TIMEOUT);
    kernel_tests::kassert_eq!(component.launches(), ROUTER_INCARNATIONS);
    kernel_tests::kassert_eq!(component.on_exhausted(), OnExhausted::Halt);

    // Слоты пришли из манифеста программы: образ и запись её каталога.
    kernel_tests::kassert_eq!(component.slots().count(), 2);
    let slots: Vec<SlotSource<'_>> = component.slots().collect();
    kernel_tests::kassert_eq!(slots[0], SlotSource::Image);
    kernel_tests::kassert_eq!(
        slots[1],
        SlotSource::File {
            path: BOOT_MOUNTS_RECORD
        }
    );
}

#[kernel_test]
fn compiled_mount_table_carries_the_boot_tree() {
    let image = image();
    let entries = image.entries().expect("image decodes");

    let record = entries
        .entry(BOOT_MOUNTS_RECORD)
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
    let corrupted = image.plan(BOOT_MOUNTS_RECORD).err();
    kernel_tests::kassert!(matches!(
        corrupted,
        Some(BootFailure::Plan(PlanDecodeError::InvalidMagic(_)))
    ));
}

#[kernel_test]
fn supervisor_restarts_the_router_with_a_fresh_namespace() {
    let image = image();
    let bytes = boot_plan();
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
    let bytes = boot_plan();
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
    let bytes = plan_bytes(OnExhausted::Halt, 1);
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
    let bytes = plan_bytes(OnExhausted::Detach, 1);
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
    let bytes = boot_plan();
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
    let bytes = boot_plan();
    let system = started(&image, &bytes);
    await_tree(&system, Path::ROOT.as_str());
    mount_crash_filesystem(&system);
    let first = await_session(&system, CRASH_MOUNT).expect("crash mount serves the prefix");
    kernel_tests::kassert!(matches!(probe(&first), Ok(Ok(_))));

    crash(&first);

    // Отказ файловой системы уносит только её процесс: маршрутизатор остаётся жив и поднимает
    // монтированию новый экземпляр, а сессии ушедшего терминальны.
    kernel_tests::kassert!(alive(&system, 0));
    kernel_tests::kassert_eq!(probe(&first), Err(IpcError::PeerClosed));
    let second = await_session(&system, CRASH_MOUNT).expect("mount got a new incarnation");
    kernel_tests::kassert!(matches!(probe(&second), Ok(Ok(_))));
}

#[kernel_test]
fn restart_limit_takes_the_mount_off_the_tree() {
    let image = image();
    let bytes = boot_plan();
    let system = started(&image, &bytes);
    await_tree(&system, Path::ROOT.as_str());
    mount_crash_filesystem(&system);

    let mut incarnations = 0;
    let mut rerouted = false;
    for _ in 0..CRASH_ATTEMPTS {
        let resolved = await_mount(&system, CRASH_MOUNT).expect("path stays covered");
        if resolved.prefix_len != CRASH_MOUNT.len() as u32 {
            rerouted = true;
            break;
        }
        incarnations += 1;
        crash(&resolved.session);
    }

    // Монтирование, исчерпавшее перезапуски, снимается с дерева имён, и его путь снова
    // накрывает корневое монтирование.
    kernel_tests::kassert_eq!(incarnations, MOUNT_INCARNATIONS);
    kernel_tests::kassert!(rerouted);
    kernel_tests::kassert!(alive(&system, 0));
}

#[kernel_test]
fn a_device_component_serves_time_over_a_minted_session() {
    let image = image();
    let device = rtc_device(&[RTC_COMPATIBLE]);
    let bytes = encode_plan(&[rtc(&device)]).expect("scenario plan encodes");
    let mut system = started(&image, &bytes);

    let session = roots(&system, 0)
        .session(PROBE_BADGE)
        .expect("rtc session minted");
    let clock = rtc::Client::new(PortTransport::client(&session)).with_wait(RTC_CALL_TIMEOUT);
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
    let device = rtc_device(&["acme,absent-clock"]);
    let bytes = encode_plan(&[rtc(&device)]).expect("scenario plan encodes");

    let failure = bring_up(&image, &bytes).err();

    kernel_tests::kassert_eq!(
        failure,
        Some(BootFailure::Device(ResolveError::DeviceNotFound))
    );
    kernel_tests::kassert_eq!(failure.map(BootFailure::stopped), Some(Stopped::Device));
}

#[kernel_test]
fn two_components_taking_one_device_line_name_the_conflict() {
    let image = image();
    let device = rtc_device(&[RTC_COMPATIBLE]);
    // Экземпляр устройства у обоих нулевой: описание платформы приводит их к
    // одному узлу, и линию получает тот, кто поднялся первым.
    let bytes = encode_plan(&[
        rtc(&device),
        ComponentSpec {
            name: RTC_TWIN_NAME,
            ..rtc(&device)
        },
    ])
    .expect("scenario plan encodes");

    let failure = bring_up(&image, &bytes).err();

    kernel_tests::kassert_eq!(failure, Some(BootFailure::DeviceBusy));
    kernel_tests::kassert_eq!(failure.map(BootFailure::stopped), Some(Stopped::Device));
}

#[kernel_test]
fn two_components_taking_one_device_window_name_the_conflict() {
    let image = image();
    let device = rtc_device(&[RTC_COMPATIBLE]);
    // Второй компонент называет одно окно с первым, линии не касаясь:
    // исключительность окна держит учёт композиции, а не ядро.
    let window = [device[1]];
    let bytes = encode_plan(&[
        rtc(&device),
        ComponentSpec {
            name: RTC_TWIN_NAME,
            slots: &window,
            ..rtc(&device)
        },
    ])
    .expect("scenario plan encodes");

    let failure = bring_up(&image, &bytes).err();

    kernel_tests::kassert_eq!(failure, Some(BootFailure::DeviceBusy));
    kernel_tests::kassert_eq!(failure.map(BootFailure::stopped), Some(Stopped::Device));
}

#[kernel_test]
fn a_service_slot_carries_the_provider_session_and_stops_before_it() {
    let image = image();
    // Потребитель стоит в плане первым, а поднимается вторым: сессию слота
    // supervisor минтит из уже поднятого провайдера.
    let bytes = encode_plan(&[
        ComponentSpec {
            executable: CONSUMER_PROGRAM,
            name: CONSUMER_NAME,
            slots: &[SlotSpec::Service { provider: 1 }],
            requires: &[1],
            after: &[],
            memory_pages: CONSUMER_MEMORY_PAGES,
            startup_timeout: ROUTER_STARTUP_TIMEOUT,
            launches: 1,
            on_exhausted: OnExhausted::Detach,
        },
        router(OnExhausted::Halt, 1),
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
    let device = rtc_device(&[RTC_COMPATIBLE]);
    // Драйвер стоит в плане первым, а поднимается вторым: порядок задаёт `after`.
    let bytes = encode_plan(&[
        ComponentSpec {
            after: &[1],
            ..rtc(&device)
        },
        router(OnExhausted::Halt, 1),
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
