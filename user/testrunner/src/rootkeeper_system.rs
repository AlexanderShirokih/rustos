//! Композиция rootkeeper по плану загрузки: чтение плана из образа, порядок
//! запуска по графу зависимостей, слоты capability и наблюдение supervisor за
//! завершением компонентов.

use alloc::{vec, vec::Vec};
use core::num::NonZeroU64;

use device_resolver::ResolveError;
use filesystem_abi::{
    AccessMode, BOOT_MOUNT_TABLE_PATH, FileSystemError, Mount, OpenFlags, Opened, Path,
    decode_mount_table, namespace, namespace_admin,
};
use ipc::wire::IpcError;
use kernel_objects::{MemoryRegion, Port, Signal, kind};
use kernel_tests::kernel_test;
use managed_component::{ComponentRoots, ManagedComponent, ManagedFailure};
use rootkeeper::{
    BootFailure, BudgetFailure, Definition, RootSystem, SHUTDOWN_EXIT_CODE, Stopped, Supervised,
    SystemImage, relay,
};
use rtc_abi::rtc;
use runtime::{MemoryAccess, PortTransport, Timeout};
use syscall::SIGNALED;
use userland::USERLAND_PAGE_SIZE;
use userland_image::{
    BOOT_PLAN_PATH, BootPlan, ComponentSpec, DeviceSpec, OnExhausted, PlanDecodeError, SlotSource,
    SlotSpec, decode_plan, encode_plan,
};

use crate::filesystem::{CALL_TIMEOUT, connect, path};

pub(crate) const BOOT_PLAN_ENTRY: &str = "/boot/testrunner/plan";

const ROUTER_EXIT_CODE: u32 = 73;

const PROBE_BADGE: NonZeroU64 = NonZeroU64::new(9).expect("badge is non-zero");

const RAM_MOUNT: &str = "/tmp";
const ROOT_MOUNT: &str = "/";

const ROUTER_PROGRAM: &str = "/boot/namespace-router";
const ROUTER_NAME: &str = "namespace-router";
const IMAGE_FS_PROGRAM: &str = "/boot/filesystem-imagefs";
const IMAGE_FS_NAME: &str = "filesystem-imagefs";
const RAM_FS_PROGRAM: &str = "/boot/filesystem-ramfs";
const RAM_FS_NAME: &str = "filesystem-ramfs";

const INIT_PROGRAM: &str = "/boot/init";
const INIT_NAME: &str = "init";
const INIT_MEMORY_PAGES: u64 = 512;

const INSTANCE_NAMES: [&str; 2] = ["names-boot", "names-spare"];
const CHAIN_NAMES: [&str; 2] = ["names-consumer", "names-consumer-tail"];
const DEPENDENT_EXIT_CODE: u32 = 57;
const ROUTER_MEMORY_PAGES: u64 = 512;
const FILE_SYSTEM_MEMORY_PAGES: u64 = 1_024;
const ROUTER_STARTUP_TIMEOUT: Timeout = Timeout::from_secs(2);

const RTC_PROGRAM: &str = "/boot/driver-pl031";
const RTC_NAME: &str = "driver-pl031";
const RTC_MEMORY_PAGES: u64 = 256;
const RTC_COMPATIBLE: &str = "arm,pl031";
const RTC_TWIN_NAME: &str = "driver-pl031-twin";

const CONSUMER_PROGRAM: &str = "/boot/namespace-consumer-fixture";
const CONSUMER_NAME: &str = "namespace-consumer-fixture";
const CONSUMER_MEMORY_PAGES: u64 = 256;

const AREA_PROGRAM: &str = "/boot/directory-consumer-fixture";
const AREA_NAME: &str = "directory-consumer-fixture";
const AREA_MEMORY_PAGES: u64 = 512;
const AREA_INCARNATIONS: u32 = 2;
const AREA_READ_EXIT_CODE: u32 = 0;

const AREA_RECORD: &str = "/boot/directory-consumer-fixture/program";

const FILE_SYSTEM_PROTOCOL: &str = "FileSystem";
const RTC_PROTOCOL: &str = "Rtc";
const FILE_SYSTEM_EXPORTS: &[&str] = &[FILE_SYSTEM_PROTOCOL];
const ROUTER_EXPORTS: &[&str] = &["Namespace", "NamespaceAdmin"];
const RTC_EXPORTS: &[&str] = &[RTC_PROTOCOL];

const FIRST_EXPORT: u8 = 0;
const ADMIN_EXPORT: u8 = 1;

const AREA_SLOTS: &[SlotSpec<'static>] = &[SlotSpec::Directory {
    provider: IMAGE_FS_NAME,
}];

const READER_PROGRAM: &str = "/boot/filesystem-consumer-fixture";
const READER_NAME: &str = "filesystem-consumer-fixture";
const READER_MEMORY_PAGES: u64 = 512;
const READER_READ_EXIT_CODE: u32 = 0;

const READER_SLOTS: &[SlotSpec<'static>] = &[SlotSpec::Service {
    provider: IMAGE_FS_NAME,
    export: FIRST_EXPORT,
}];
const READER_SOURCES: &[SlotSource<'static>] = &[SlotSource::Service {
    provider: IMAGE_FS_NAME,
    export: FIRST_EXPORT,
}];

const EXTENDER_PROGRAM: &str = "/boot/supervisor-consumer-fixture";
const EXTENDER_NAME: &str = "supervisor-consumer-fixture";
const EXTENDER_MEMORY_PAGES: u64 = 512;
const EXTENDER_RAISED_EXIT_CODE: u32 = 0;

const EXTENDER_SLOTS: &[SlotSpec<'static>] = &[
    SlotSpec::Service {
        provider: IMAGE_FS_NAME,
        export: FIRST_EXPORT,
    },
    SlotSpec::Supervisor,
];

const RAM_FS_RECORD: &str = "/boot/filesystem-ramfs/program";
const RTC_RECORD: &str = "/boot/driver-pl031/program";
const DEFINED_NAME: &str = "extended-ramfs";

const CYCLE_NAMES: [&str; 2] = ["cycle-head", "cycle-tail"];
const CYCLE_HEAD_EDGE: [&str; 1] = [CYCLE_NAMES[1]];
const CYCLE_TAIL_EDGE: [&str; 1] = [CYCLE_NAMES[0]];

const ORPHAN_NAME: &str = "orphan";
const ORPHAN_SLOTS: &[SlotSpec<'static>] = &[SlotSpec::Service {
    provider: "ghost",
    export: FIRST_EXPORT,
}];

const GREEDY_NAME: &str = "greedy";
const GREEDY_MEMORY_PAGES: u64 = u64::MAX / 2;

const EXTENSION_TURNS: usize = 32;

const STAGE_ONE_OPERATIONS: usize = 2;

const STAGE_ONE_RECORD: &str = "/boot/init/stage1/filesystem-ramfs";

const MOUNTS_PROGRAM: &str = "/boot/control-consumer-fixture";
const MOUNTS_NAME: &str = "control-consumer-fixture";
const MOUNTS_MEMORY_PAGES: u64 = 256;
const MOUNTS_EXIT_CODE: u32 = 61;

const MOUNTS_SLOTS: &[SlotSpec<'static>] = &[SlotSpec::Service {
    provider: ROUTER_NAME,
    export: ADMIN_EXPORT,
}];

const BOOT_TREE: &[Mount<'static>] = &[
    Mount {
        path: "/",
        instance: IMAGE_FS_NAME,
    },
    Mount {
        path: RAM_MOUNT,
        instance: RAM_FS_NAME,
    },
];

const ROUTER_INCARNATIONS: u32 = 3;

const ROUTER_ATTEMPTS: usize = 6;

const RESTART_ATTEMPTS: usize = 100;
const RESTART_TURN: Timeout = Timeout::from_ns(20_000_000);

const RTC_CALL_TIMEOUT: Timeout = Timeout::from_secs(15);

const CONSUMER_TURN: Timeout = Timeout::from_millis(200);

const DEVICE_EXIT_CODE: u32 = 41;

const USERLAND_SLOT: &[SlotSpec<'static>] = &[SlotSpec::Userland];

const INIT_SLOTS: &[SlotSpec<'static>] = &[
    SlotSpec::Userland,
    SlotSpec::Service {
        provider: IMAGE_FS_NAME,
        export: FIRST_EXPORT,
    },
    SlotSpec::Service {
        provider: ROUTER_NAME,
        export: ADMIN_EXPORT,
    },
    SlotSpec::Supervisor,
];
const INIT_SOURCES: &[SlotSource<'static>] = &[
    SlotSource::Userland,
    SlotSource::Service {
        provider: IMAGE_FS_NAME,
        export: FIRST_EXPORT,
    },
    SlotSource::Service {
        provider: ROUTER_NAME,
        export: ADMIN_EXPORT,
    },
    SlotSource::Supervisor,
];

fn router(on_exhausted: OnExhausted, launches: u32) -> ComponentSpec<'static> {
    ComponentSpec {
        executable: ROUTER_PROGRAM,
        name: ROUTER_NAME,
        slots: &[],
        exports: ROUTER_EXPORTS,
        after: &[],
        memory_pages: ROUTER_MEMORY_PAGES,
        startup_timeout: ROUTER_STARTUP_TIMEOUT,
        launches,
        on_exhausted,
        establishes_sessions: false,
    }
}

fn image_filesystem() -> ComponentSpec<'static> {
    ComponentSpec {
        executable: IMAGE_FS_PROGRAM,
        name: IMAGE_FS_NAME,
        slots: USERLAND_SLOT,
        exports: FILE_SYSTEM_EXPORTS,
        after: &[],
        memory_pages: FILE_SYSTEM_MEMORY_PAGES,
        startup_timeout: ROUTER_STARTUP_TIMEOUT,
        launches: 1,
        on_exhausted: OnExhausted::Halt,
        establishes_sessions: true,
    }
}

fn ram_filesystem() -> ComponentSpec<'static> {
    ComponentSpec {
        executable: RAM_FS_PROGRAM,
        name: RAM_FS_NAME,
        slots: &[],
        exports: FILE_SYSTEM_EXPORTS,
        after: &[],
        memory_pages: FILE_SYSTEM_MEMORY_PAGES,
        startup_timeout: ROUTER_STARTUP_TIMEOUT,
        launches: 1,
        on_exhausted: OnExhausted::Halt,
        establishes_sessions: true,
    }
}

fn boot_init() -> ComponentSpec<'static> {
    ComponentSpec {
        executable: INIT_PROGRAM,
        name: INIT_NAME,
        slots: INIT_SLOTS,
        exports: &[],
        after: &[],
        memory_pages: INIT_MEMORY_PAGES,
        startup_timeout: ROUTER_STARTUP_TIMEOUT,
        launches: 1,
        on_exhausted: OnExhausted::Halt,
        establishes_sessions: false,
    }
}

fn plan_bytes(on_exhausted: OnExhausted, launches: u32) -> Vec<u8> {
    // В плане нет файловой системы `/tmp`, потому что `init` поднимает её записью ступени 1.
    encode_plan(&[
        router(on_exhausted, launches),
        image_filesystem(),
        boot_init(),
    ])
    .expect("scenario plan encodes")
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

fn reader() -> ComponentSpec<'static> {
    ComponentSpec {
        executable: READER_PROGRAM,
        name: READER_NAME,
        slots: READER_SLOTS,
        exports: &[],
        after: &[],
        memory_pages: READER_MEMORY_PAGES,
        startup_timeout: ROUTER_STARTUP_TIMEOUT,
        launches: 1,
        on_exhausted: OnExhausted::Detach,
        establishes_sessions: false,
    }
}

fn mount_admin() -> ComponentSpec<'static> {
    ComponentSpec {
        executable: MOUNTS_PROGRAM,
        name: MOUNTS_NAME,
        slots: MOUNTS_SLOTS,
        exports: &[],
        after: &[],
        memory_pages: MOUNTS_MEMORY_PAGES,
        startup_timeout: ROUTER_STARTUP_TIMEOUT,
        launches: 1,
        on_exhausted: OnExhausted::Detach,
        establishes_sessions: false,
    }
}

fn rtc<'a>(slots: &'a [SlotSpec<'a>]) -> ComponentSpec<'a> {
    ComponentSpec {
        executable: RTC_PROGRAM,
        name: RTC_NAME,
        slots,
        exports: RTC_EXPORTS,
        after: &[],
        memory_pages: RTC_MEMORY_PAGES,
        startup_timeout: ROUTER_STARTUP_TIMEOUT,
        launches: 1,
        on_exhausted: OnExhausted::Detach,
        establishes_sessions: false,
    }
}

fn area_consumer() -> ComponentSpec<'static> {
    ComponentSpec {
        executable: AREA_PROGRAM,
        name: AREA_NAME,
        slots: AREA_SLOTS,
        exports: &[],
        after: &[],
        memory_pages: AREA_MEMORY_PAGES,
        startup_timeout: ROUTER_STARTUP_TIMEOUT,
        launches: AREA_INCARNATIONS,
        on_exhausted: OnExhausted::Detach,
        establishes_sessions: false,
    }
}

fn extender() -> ComponentSpec<'static> {
    ComponentSpec {
        executable: EXTENDER_PROGRAM,
        name: EXTENDER_NAME,
        slots: EXTENDER_SLOTS,
        exports: &[],
        after: &[],
        memory_pages: EXTENDER_MEMORY_PAGES,
        startup_timeout: ROUTER_STARTUP_TIMEOUT,
        launches: 1,
        on_exhausted: OnExhausted::Detach,
        establishes_sessions: false,
    }
}

fn image() -> SystemImage {
    SystemImage::acquire(crate::bootstrap_handle()).expect("userland image is mapped")
}

fn program_window(image: &SystemImage, entry: &str) -> MemoryRegion {
    let entries = image.entries().expect("image decodes");
    let record = entries.entry(entry).expect("image carries the program");
    let size = (record.data().len() as u64).div_ceil(USERLAND_PAGE_SIZE) * USERLAND_PAGE_SIZE;
    image
        .region()
        .slice(record.data_offset(), size, MemoryAccess::READ)
        .expect("program window derives from the image region")
}

fn defined_ramfs() -> ComponentSpec<'static> {
    ComponentSpec {
        name: DEFINED_NAME,
        ..ram_filesystem()
    }
}

fn caller() -> NonZeroU64 {
    relay::component_badge(0)
}

fn other_caller() -> NonZeroU64 {
    relay::component_badge(1)
}

fn caller_outside_the_composition() -> NonZeroU64 {
    relay::component_badge(u16::MAX)
}

fn definition(image: &SystemImage, entry: &str, component: ComponentSpec<'_>) -> Definition {
    let record = encode_plan(&[component]).expect("component record encodes");
    Definition::new(&record, program_window(image, entry))
}

pub(crate) fn raise_stage_one(system: &mut RootSystem<'_>, relay: syscall::Handle<kind::Thread>) {
    // Оборот обслуживает по одной операции транзакции, и `init` подаёт их две: набор ступени 1 и
    // сессию его файловой системы под монтирование. Исходы компонентов приходят тем же оборотом и
    // не идут в счёт.
    let mut served = 0;
    for _ in 0..EXTENSION_TURNS {
        if served == STAGE_ONE_OPERATIONS {
            return;
        }
        match system.supervise_turn(relay) {
            Supervised::Extended => served += 1,
            Supervised::Stopped(_) => return,
            _ => {}
        }
    }
}

fn await_turn(system: &mut RootSystem<'_>) -> Supervised {
    // Оборот обслуживает операции транзакции по одной, и исход компонента приходит следом.
    for _ in 0..EXTENSION_TURNS {
        match system.supervise_turn(relay_thread()) {
            Supervised::Extended => {}
            outcome => return outcome,
        }
    }
    panic!("supervisor turn past the transaction is not reached")
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

fn relay_thread() -> syscall::Handle<kind::Thread> {
    crate::log_relay::shared_relay_thread().expect("shared relay is running")
}

fn scenario_relay_thread() -> syscall::Handle<kind::Thread> {
    crate::log_relay::scenario_relay_thread().expect("scenario relay is running")
}

fn try_mount(system: &RootSystem<'_>, prefix: &str) -> Result<Port, FileSystemError> {
    let namespace = roots(system, 0)
        .session(usize::from(FIRST_EXPORT), PROBE_BADGE)
        .expect("namespace session minted");
    // Отказ транспорта на пути к ещё не обслуживающему порт маршрутизатору неотличим
    // для клиента от неотвечающего монтирования.
    let mut resolved = namespace::Client::new(PortTransport::client(&namespace))
        .with_wait(CALL_TIMEOUT)
        .resolve(path(prefix))
        .map_err(|_| FileSystemError::Unresponsive)?;
    let session = resolved.value?;
    let session = resolved
        .claim(session)
        .ok_or(FileSystemError::Unresponsive)?;
    Ok(Port::from(session))
}

fn await_tree(system: &RootSystem<'_>, prefix: &str) -> Port {
    // Маршрутизатор сам поднимает дерево имён. Стартовый кадр только доставлен, поэтому путь
    // становится накрытым его оборотами.
    for _ in 0..RESTART_ATTEMPTS {
        match try_mount(system, prefix) {
            Ok(session) => return session,
            Err(_) => pause(RESTART_TURN),
        }
    }
    panic!("boot tree is not mounted")
}

fn await_unmounted(system: &RootSystem<'_>, prefix: &str) {
    // Потребитель снимает монтирование своим оборотом, и путь перестаёт быть накрытым.
    for _ in 0..RESTART_ATTEMPTS {
        if matches!(try_mount(system, prefix), Err(FileSystemError::NotFound)) {
            return;
        }
        pause(RESTART_TURN);
    }
    panic!("boot mount is not removed")
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

pub(crate) fn pause(timeout: Timeout) {
    let timer = Signal::create().expect("pause timer");
    let _ = timer.wait(SIGNALED, timeout);
}

fn probe(session: &Port) -> Result<Result<Opened, FileSystemError>, IpcError> {
    connect(session)
        .open(
            path(Path::ROOT.as_str()),
            AccessMode::Read,
            OpenFlags::DIRECTORY,
        )
        .map(|response| response.value)
}

fn terminate_router(system: &RootSystem<'_>) -> bool {
    terminate(system, 0, ROUTER_EXIT_CODE)
}

#[kernel_test]
fn two_instances_of_one_program_run_under_their_own_names() {
    let image = image();
    let bytes = encode_plan(&[
        ComponentSpec {
            name: INSTANCE_NAMES[0],
            ..ram_filesystem()
        },
        ComponentSpec {
            name: INSTANCE_NAMES[1],
            ..ram_filesystem()
        },
    ])
    .expect("scenario plan encodes");
    let decoded = plan(&bytes);
    // Rootkeeper сам выводит имена источников журнала из записей набора, поэтому строка приходит
    // под именем экземпляра, а не под путём его программы.
    crate::log_relay::start_component_relay();

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
            .any(|source| source.split('/').next() == Some(RAM_FS_NAME))
    );

    system.shutdown();
    crate::log_relay::stop_component_relay();
}

#[kernel_test]
fn compiled_plan_carries_the_boot_composition() {
    let image = image();

    let plan = image.plan(BOOT_PLAN_ENTRY).expect("compiled plan decodes");

    kernel_tests::kassert_eq!(plan.len(), 6);
    let router = plan.component(0).expect("plan carries the router");
    kernel_tests::kassert_eq!(router.executable(), ROUTER_PROGRAM);
    kernel_tests::kassert_eq!(router.name(), ROUTER_NAME);
    kernel_tests::kassert_eq!(router.memory_pages(), ROUTER_MEMORY_PAGES);
    kernel_tests::kassert_eq!(router.startup_timeout(), ROUTER_STARTUP_TIMEOUT);
    kernel_tests::kassert_eq!(router.launches(), ROUTER_INCARNATIONS);
    kernel_tests::kassert_eq!(router.on_exhausted(), OnExhausted::Halt);
    // Маршрутизатор не учитывает файловые системы, потому что у него в композиции нет слотов и
    // рёбер.
    kernel_tests::kassert!(router.slots().next().is_none());
    kernel_tests::kassert!(router.after().next().is_none());

    let imagefs = plan
        .component(1)
        .expect("plan carries the root file system");
    kernel_tests::kassert_eq!(imagefs.executable(), IMAGE_FS_PROGRAM);
    kernel_tests::kassert_eq!(imagefs.on_exhausted(), OnExhausted::Halt);
    let slots: Vec<SlotSource<'_>> = imagefs.slots().collect();
    kernel_tests::kassert_eq!(slots, [SlotSource::Userland]);

    // Ступень 0 сжата до корня дерева имён, потому что в плане нет файловой системы `/tmp`.
    kernel_tests::kassert!(
        !plan
            .components()
            .any(|component| component.executable() == RAM_FS_PROGRAM)
    );

    // В композиции несколько экспортёров `FileSystem`, и слот потребителя связан с тем экземпляром,
    // которого назвал его манифест.
    let reader = plan.component(4).expect("plan carries the file reader");
    kernel_tests::kassert_eq!(reader.executable(), READER_PROGRAM);
    kernel_tests::kassert_eq!(reader.slots().collect::<Vec<_>>(), READER_SOURCES);

    // `init` определяет политику загрузки. Корень он берёт слотом по имени экземпляра, таблицу
    // монтирований - записью своего каталога, а ступень 1 подаёт слотом супервизора.
    let init = plan.component(5).expect("plan carries init");
    kernel_tests::kassert_eq!(init.executable(), INIT_PROGRAM);
    kernel_tests::kassert_eq!(init.name(), INIT_NAME);
    kernel_tests::kassert_eq!(init.slots().collect::<Vec<_>>(), INIT_SOURCES);
}

#[kernel_test]
fn compiled_mount_table_carries_the_boot_tree() {
    let image = image();
    let entries = image.entries().expect("image decodes");

    let record = entries
        .entry(BOOT_MOUNT_TABLE_PATH)
        .expect("image carries the compiled mount table");
    let table = decode_mount_table(record.data()).expect("compiled table decodes");

    kernel_tests::kassert_eq!(table.len(), BOOT_TREE.len());
    kernel_tests::kassert_eq!(table.get(0), Some(BOOT_TREE[0]));
    kernel_tests::kassert_eq!(table.get(1), Some(BOOT_TREE[1]));
}

#[kernel_test]
fn boot_without_a_plan_record_names_its_failure() {
    let image = image();

    // Образ прогона содержит план по своему пути, а в нём нет записи загрузочного плана.
    let missing = image.plan(BOOT_PLAN_PATH).err();
    kernel_tests::kassert_eq!(missing, Some(BootFailure::PlanMissing));
    kernel_tests::kassert_eq!(missing.map(BootFailure::stopped), Some(Stopped::BadImage));
}

#[kernel_test]
fn boot_with_a_corrupted_plan_record_names_its_failure() {
    let image = image();

    // Запись программы не является планом, поэтому декодер отвергает её байты.
    let corrupted = image.plan(BOOT_MOUNT_TABLE_PATH).err();
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
    raise_stage_one(&mut system, relay_thread());
    let session = await_tree(&system, RAM_MOUNT);
    let init = system.instance(INIT_NAME).expect("plan carries init");
    kernel_tests::kassert!(terminate_router(&system));

    let supervised = system.supervise_turn(relay_thread());

    kernel_tests::kassert_eq!(
        supervised,
        Supervised::Restarted {
            exit_code: Some(ROUTER_EXIT_CODE),
            dependents: 1,
        }
    );
    // Сессия принадлежит файловой системе, а не маршрутизатору, поэтому его смена не отзывает её.
    kernel_tests::kassert!(matches!(probe(&session), Ok(Ok(_))));
    kernel_tests::kassert_eq!(managed(&system, 0).generation(), 2);
    // Монтирования принадлежат incarnation, и держатель слота завершился вместе с ней. Его
    // смена наполняет свежее дерево имён.
    kernel_tests::kassert_eq!(managed(&system, init).generation(), 2);
    // Статически связанный корень монтируется слотом свежей incarnation.
    kernel_tests::kassert!(matches!(probe(&await_tree(&system, ROOT_MOUNT)), Ok(Ok(_))));
    // Ступень 1 осталась в композиции, и оборот транзакции отдаёт сессию её файловой системы.
    raise_stage_one(&mut system, relay_thread());
    kernel_tests::kassert!(matches!(probe(&await_tree(&system, RAM_MOUNT)), Ok(Ok(_))));
    system.shutdown();
}

#[kernel_test]
fn a_consumer_taken_down_with_its_provider_keeps_its_launch_limit() {
    let image = image();
    // Потребитель без перезапусков, потому что собственный отказ снял бы композицию исходом `halt`.
    let bytes = encode_plan(&[
        router(OnExhausted::Halt, ROUTER_INCARNATIONS),
        ComponentSpec {
            name: CHAIN_NAMES[0],
            after: &[ROUTER_NAME],
            launches: 1,
            on_exhausted: OnExhausted::Halt,
            ..ram_filesystem()
        },
    ])
    .expect("scenario plan encodes");
    let mut system = started(&image, &bytes);

    for incarnation in 2..=ROUTER_INCARNATIONS {
        kernel_tests::kassert!(terminate_router(&system));
        let supervised = system.supervise_turn(relay_thread());
        kernel_tests::kassert_eq!(
            supervised,
            Supervised::Restarted {
                exit_code: Some(ROUTER_EXIT_CODE),
                dependents: 1,
            }
        );
        // Предел считает запуски, отнесённые к отказам самого компонента, поэтому чужие перезапуски
        // не тратят его.
        kernel_tests::kassert_eq!(managed(&system, 1).generation(), incarnation);
        kernel_tests::kassert!(alive(&system, 1));
    }

    system.shutdown();
}

#[kernel_test]
fn a_provider_restart_reaches_the_consumer_of_its_consumer() {
    let image = image();
    let bytes = encode_plan(&[
        router(OnExhausted::Halt, ROUTER_INCARNATIONS),
        ComponentSpec {
            name: CHAIN_NAMES[0],
            after: &[ROUTER_NAME],
            ..ram_filesystem()
        },
        ComponentSpec {
            name: CHAIN_NAMES[1],
            after: &[CHAIN_NAMES[0]],
            ..ram_filesystem()
        },
    ])
    .expect("scenario plan encodes");
    let mut system = started(&image, &bytes);
    kernel_tests::kassert_eq!(system.start_order(), &[0, 1, 2][..]);

    kernel_tests::kassert!(terminate_router(&system));
    let supervised = system.supervise_turn(relay_thread());

    // Снятие транзитивно. Потребитель потребителя завершается с провайдером и поднимается его
    // сменой.
    kernel_tests::kassert_eq!(
        supervised,
        Supervised::Restarted {
            exit_code: Some(ROUTER_EXIT_CODE),
            dependents: 2,
        }
    );
    kernel_tests::kassert_eq!(managed(&system, 1).generation(), 2);
    kernel_tests::kassert_eq!(managed(&system, 2).generation(), 2);
    kernel_tests::kassert!(alive(&system, 2));
    system.shutdown();
}

#[kernel_test]
fn a_consumer_departure_leaves_its_provider_alone() {
    let image = image();
    let bytes = encode_plan(&[
        router(OnExhausted::Halt, ROUTER_INCARNATIONS),
        ComponentSpec {
            name: CHAIN_NAMES[0],
            after: &[ROUTER_NAME],
            on_exhausted: OnExhausted::Detach,
            ..ram_filesystem()
        },
    ])
    .expect("scenario plan encodes");
    let mut system = started(&image, &bytes);

    kernel_tests::kassert!(terminate(&system, 1, DEPENDENT_EXIT_CODE));
    let supervised = system.supervise_turn(relay_thread());

    // Ребро зависимости одностороннее, потому что завершение потребителя не касается провайдера.
    kernel_tests::kassert_eq!(
        supervised,
        Supervised::Abandoned {
            exit_code: Some(DEPENDENT_EXIT_CODE),
            dependents: 0,
        }
    );
    kernel_tests::kassert_eq!(managed(&system, 0).generation(), 1);
    kernel_tests::kassert!(alive(&system, 0));
    system.shutdown();
}

#[kernel_test]
fn supervisor_stops_the_composition_at_the_router_restart_limit() {
    let image = image();
    let bytes = boot_plan();
    let mut system = started(&image, &bytes);
    raise_stage_one(&mut system, relay_thread());

    let mut supervised = Supervised::Extended;
    for _ in 0..ROUTER_ATTEMPTS {
        kernel_tests::kassert!(terminate_router(&system));
        // Смена маршрутизатора поднимает и владельца политики загрузки, и его транзакция
        // приходит тем же оборотом.
        supervised = await_turn(&mut system);
        if matches!(supervised, Supervised::Stopped(_)) {
            break;
        }
    }

    // Исчерпав предел запусков, композиция останавливается и сохраняет код выхода последней
    // incarnation.
    kernel_tests::kassert_eq!(supervised, Supervised::Stopped(Stopped::Subsystem));
    // Предел запусков остановил композицию, потому что рестарт, отказавший раньше, оставил бы
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
    raise_stage_one(&mut system, relay_thread());
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
    raise_stage_one(&mut system, relay_thread());
    kernel_tests::kassert!(terminate_router(&system));

    let supervised = system.supervise_turn(relay_thread());

    // Некритический компонент снимается с композиции, а композиция сама остаётся поднятой, потому
    // что её поток пересылки жив и продолжает выдавать сессии.
    kernel_tests::kassert_eq!(
        supervised,
        Supervised::Abandoned {
            exit_code: Some(ROUTER_EXIT_CODE),
            dependents: 1,
        }
    );
    kernel_tests::kassert!(managed(&system, 0).running().is_none());
    // В композиции больше нет провайдера, и его потребитель остаётся снятым, потому что взять слот
    // у завершившегося некому.
    let init = system.instance(INIT_NAME).expect("plan carries init");
    kernel_tests::kassert!(managed(&system, init).running().is_none());
    system.shutdown();
}

#[kernel_test]
fn router_restart_keeps_file_system_components_and_their_sessions() {
    let image = image();
    let bytes = boot_plan();
    let mut system = started(&image, &bytes);
    raise_stage_one(&mut system, relay_thread());
    let session = await_tree(&system, RAM_MOUNT);
    kernel_tests::kassert!(matches!(probe(&session), Ok(Ok(_))));
    let raised = system
        .instance(RAM_FS_NAME)
        .expect("stage 1 carries the file system of /tmp");

    kernel_tests::kassert!(terminate_router(&system));
    system.supervise_turn(relay_thread());

    // Файловые системы принадлежат композиции rootkeeper, поэтому смена router не меняет их
    // generation и не отзывает уже выданную сессию. Компонент ступени 1 находится в ней наравне с
    // плановым.
    kernel_tests::kassert_eq!(managed(&system, 1).generation(), 1);
    kernel_tests::kassert_eq!(managed(&system, raised).generation(), 1);
    kernel_tests::kassert!(matches!(probe(&session), Ok(Ok(_))));
    system.shutdown();
}

#[kernel_test]
fn root_file_system_failure_is_seen_by_the_supervisor() {
    let image = image();
    let bytes = boot_plan();
    let mut system = started(&image, &bytes);
    // Отдельный компонент наполняет дерево, и `bring_up` не ждёт его монтирований. Корневая
    // файловая система, снятая до конца монтирования, уводит и маршрутизатор.
    raise_stage_one(&mut system, relay_thread());
    drop(await_tree(&system, RAM_MOUNT));
    kernel_tests::kassert!(terminate(&system, 1, ROUTER_EXIT_CODE));

    let supervised = system.supervise_turn(relay_thread());

    kernel_tests::kassert_eq!(supervised, Supervised::Stopped(Stopped::Subsystem));
    kernel_tests::kassert_eq!(
        managed(&system, 1).failure(),
        Some(ManagedFailure::Exit(ROUTER_EXIT_CODE))
    );
    system.shutdown();
}

#[kernel_test]
fn a_device_component_serves_time_over_a_minted_session() {
    let image = image();
    let device = rtc_device(&[RTC_COMPATIBLE]);
    let bytes = encode_plan(&[rtc(&device)]).expect("scenario plan encodes");
    let mut system = started(&image, &bytes);

    let session = roots(&system, 0)
        .session(usize::from(FIRST_EXPORT), PROBE_BADGE)
        .expect("rtc session minted");
    let clock = rtc::Client::new(PortTransport::client(&session)).with_wait(RTC_CALL_TIMEOUT);
    let now = clock.now().expect("rtc answers the time").value;
    // Будильник устройства снимается реальным прерыванием линии, сминченной
    // супервизором по описанию платформы.
    let ticked = clock.tick().expect("rtc alarm fires").value;

    kernel_tests::kassert!(ticked >= now);
    drop(session);
    // Завершение процесса драйвера освобождает линию устройства. Следующий минт той же линии иначе
    // получает AlreadyBound.
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
    // Экземпляр устройства у обоих нулевой, поэтому описание платформы приводит их к одному узлу, и
    // тот, кто поднялся первым, получает линию.
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
    // Второй компонент называет одно окно с первым, линии не касаясь, потому что исключительность
    // окна держит учёт композиции, а не ядро.
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
    // Потребитель стоит в плане первым, а поднимается вторым, потому что supervisor минтит сессию
    // слота из уже поднятого провайдера.
    let bytes = encode_plan(&[
        ComponentSpec {
            executable: CONSUMER_PROGRAM,
            name: CONSUMER_NAME,
            slots: &[SlotSpec::Service {
                provider: ROUTER_NAME,
                export: FIRST_EXPORT,
            }],
            exports: &[],
            after: &[],
            memory_pages: CONSUMER_MEMORY_PAGES,
            startup_timeout: ROUTER_STARTUP_TIMEOUT,
            launches: 1,
            on_exhausted: OnExhausted::Detach,
            establishes_sessions: false,
        },
        router(OnExhausted::Halt, 1),
        image_filesystem(),
        ram_filesystem(),
    ])
    .expect("scenario plan encodes");
    let mut system = started(&image, &bytes);

    kernel_tests::kassert_eq!(system.start_order(), &[1, 2, 3, 0][..]);
    // Потребитель разрешает путь по сессии слота, пока провайдер жив. Отказ первого resolve увёл бы
    // его собственным кодом.
    pause(CONSUMER_TURN);
    kernel_tests::kassert!(alive(&system, 0));

    system.shutdown();

    // Остановка обратна порядку запуска. Потребитель снят раньше провайдера и не успел увидеть
    // закрытия его сессии.
    kernel_tests::kassert_eq!(
        managed(&system, 0).failure(),
        Some(ManagedFailure::Exit(SHUTDOWN_EXIT_CODE))
    );
}

#[kernel_test]
fn a_service_slot_of_a_file_system_carries_a_session_it_established_itself() {
    let image = image();
    // Потребитель стоит в плане первым, а поднимается вторым, потому что supervisor берёт сессию
    // слота у уже поднятой файловой системы её операцией установления.
    let bytes = encode_plan(&[reader(), image_filesystem()]).expect("scenario plan encodes");
    let mut system = started(&image, &bytes);
    kernel_tests::kassert_eq!(system.start_order(), &[1, 0][..]);

    // Значки супервизора не попадают на service-порт файловой системы, потому что она сама создала
    // сессию слота, а сминченная капа кадром сессии не создаёт.
    let files = roots(&system, 1);
    for badge in [PROBE_BADGE, relay::component_badge(0)] {
        let minted = files
            .session(usize::from(FIRST_EXPORT), badge)
            .expect("session minted");
        kernel_tests::kassert_eq!(probe(&minted), Ok(Err(FileSystemError::NoSession)));
    }

    // Потребитель прочитал запись образа сессией слота. Он уносит прочие исходы своим кодом выхода.
    let read = system.supervise_turn(relay_thread());
    kernel_tests::kassert_eq!(
        read,
        Supervised::Abandoned {
            exit_code: Some(READER_READ_EXIT_CODE),
            dependents: 0,
        }
    );

    system.shutdown();
}

#[kernel_test]
fn a_service_slot_names_its_provider_among_two_exporters() {
    let image = image();
    // В композиции двое экспортёров `FileSystem`, и слот называет третью запись именем экземпляра.
    // Она даёт сессию слота.
    let bytes = encode_plan(&[reader(), ram_filesystem(), image_filesystem()])
        .expect("scenario plan encodes");
    let mut system = started(&image, &bytes);
    kernel_tests::kassert_eq!(system.start_order(), &[1, 2, 0][..]);

    // Запись образа лежит в imagefs, и код выхода потребителя подтверждает, что сессия
    // слота пришла от него.
    let read = system.supervise_turn(relay_thread());
    kernel_tests::kassert_eq!(
        read,
        Supervised::Abandoned {
            exit_code: Some(READER_READ_EXIT_CODE),
            dependents: 0,
        }
    );

    system.shutdown();
}

#[kernel_test]
fn a_service_slot_naming_an_instance_outside_the_composition_names_the_boot_failure() {
    let image = image();
    let bytes = encode_plan(&[
        ComponentSpec {
            slots: &[SlotSpec::Service {
                provider: "ghost",
                export: FIRST_EXPORT,
            }],
            ..reader()
        },
        image_filesystem(),
    ])
    .expect("scenario plan encodes");

    let failure = bring_up(&image, &bytes).err();

    kernel_tests::kassert_eq!(failure, Some(BootFailure::Provider));
    kernel_tests::kassert_eq!(failure.map(BootFailure::stopped), Some(Stopped::Startup));
}

#[kernel_test]
fn a_service_slot_of_the_admin_export_reaches_the_admin_protocol_of_its_provider() {
    let image = image();
    let bytes = encode_plan(&[
        router(OnExhausted::Halt, 1),
        image_filesystem(),
        ComponentSpec {
            after: &[INIT_NAME],
            ..mount_admin()
        },
        boot_init(),
    ])
    .expect("scenario plan encodes");
    let mut system = started(&image, &bytes);
    kernel_tests::kassert_eq!(system.start_order(), &[0, 1, 3, 2][..]);
    // `init` монтирует префикс потребителя сессией компонента ступени 1.
    raise_stage_one(&mut system, relay_thread());

    // Потребитель зовёт `NamespaceAdmin::unmount` капабилити слота второго экспорта, поэтому
    // снятое монтирование видно на порту первого экспорта того же провайдера, а он уносит отказ
    // вызова собственным кодом выхода.
    await_unmounted(&system, RAM_MOUNT);

    // Экспорт `Namespace` административной операции не разбирает, и держатель его сессии
    // кадр административного ordinal провайдеру не проведёт.
    let namespace = roots(&system, 0)
        .session(usize::from(FIRST_EXPORT), PROBE_BADGE)
        .expect("namespace session minted");
    let refused = namespace_admin::Client::new(PortTransport::client(&namespace))
        .with_wait(CALL_TIMEOUT)
        .unmount(path(ROOT_MOUNT))
        .map(|response| response.value);
    kernel_tests::kassert_eq!(refused, Err(IpcError::PeerClosed));

    kernel_tests::kassert!(terminate(&system, 2, MOUNTS_EXIT_CODE));
    let gone = await_turn(&mut system);
    kernel_tests::kassert_eq!(
        gone,
        Supervised::Abandoned {
            exit_code: Some(MOUNTS_EXIT_CODE),
            dependents: 0,
        }
    );

    // Завершение значка потребителя порт экспорта провайдера не закрывает, и маршрутизатор
    // продолжает разбирать пути.
    pause(CONSUMER_TURN);
    kernel_tests::kassert!(alive(&system, 0));
    kernel_tests::kassert!(try_mount(&system, ROOT_MOUNT).is_ok());

    system.shutdown();
}

#[kernel_test]
fn a_directory_slot_carries_the_component_area_to_every_incarnation() {
    let image = image();
    // Потребитель стоит в плане первым, а поднимается вторым, потому что supervisor берёт область
    // его каталога у названной слотом файловой системы, и ей не нужно дерева имён.
    let bytes = encode_plan(&[area_consumer(), image_filesystem()]).expect("scenario plan encodes");
    let mut system = started(&image, &bytes);
    kernel_tests::kassert_eq!(system.start_order(), &[1, 0][..]);

    // Компонент читает свою запись сессией каталога и сам проверяет, что область не выражает
    // путь наружу. Он уносит прочие исходы своим кодом выхода.
    let first = await_turn(&mut system);
    kernel_tests::kassert_eq!(
        first,
        Supervised::Restarted {
            exit_code: Some(AREA_READ_EXIT_CODE),
            dependents: 0,
        }
    );

    // Область выдаётся заново. Следующая incarnation получает свежую сессию корневой ФС.
    let second = await_turn(&mut system);
    kernel_tests::kassert_eq!(
        second,
        Supervised::Abandoned {
            exit_code: Some(AREA_READ_EXIT_CODE),
            dependents: 0,
        }
    );

    system.shutdown();
}

#[kernel_test]
fn a_transaction_raises_a_component_from_the_program_region_of_its_definer() {
    let image = image();
    let bytes = encode_plan(&[extender(), image_filesystem()]).expect("scenario plan encodes");
    let mut system = started(&image, &bytes);
    kernel_tests::kassert_eq!(system.start_order(), &[1, 0][..]);

    // Компонент берёт байты программы регионом своей файловой сессии, определяет её экземпляром
    // композиции и работает с сессией поднятого компонента. Он уносит прочие исходы собственным
    // кодом выхода.
    let raised = await_turn(&mut system);

    kernel_tests::kassert_eq!(
        raised,
        Supervised::Abandoned {
            exit_code: Some(EXTENDER_RAISED_EXIT_CODE),
            dependents: 0,
        }
    );
    let defined = system
        .instance(DEFINED_NAME)
        .expect("the transaction added the component to the composition");
    kernel_tests::kassert!(alive(&system, defined));
    // Порядок пересчитан по объединению рёбер. Компонент набора без зависимостей встал раньше
    // потребителя файловой системы, который определил его.
    kernel_tests::kassert_eq!(system.start_order(), &[1, 2, 0][..]);

    system.shutdown();
}

#[kernel_test]
fn a_transaction_naming_a_taken_instance_raises_nothing() {
    let image = image();
    let bytes = encode_plan(&[ram_filesystem()]).expect("scenario plan encodes");
    let mut system = started(&image, &bytes);

    let refused = system.extend(
        caller(),
        vec![definition(
            &image,
            RAM_FS_RECORD,
            ComponentSpec {
                name: RAM_FS_NAME,
                ..ram_filesystem()
            },
        )],
    );

    kernel_tests::kassert_eq!(refused, Err(BootFailure::Instance));
    kernel_tests::kassert!(system.component(1).is_none());
    kernel_tests::kassert_eq!(system.start_order(), &[0][..]);
    kernel_tests::kassert!(alive(&system, 0));
    system.shutdown();
}

#[kernel_test]
fn a_transaction_whose_edges_close_a_cycle_raises_nothing() {
    let image = image();
    let bytes = encode_plan(&[ram_filesystem()]).expect("scenario plan encodes");
    let mut system = started(&image, &bytes);

    // Компоненты набора называют друг друга, поэтому порядок запуска не выводится из объединения
    // рёбер.
    let refused = system.extend(
        caller(),
        vec![
            definition(
                &image,
                RAM_FS_RECORD,
                ComponentSpec {
                    name: CYCLE_NAMES[0],
                    after: &CYCLE_HEAD_EDGE,
                    ..ram_filesystem()
                },
            ),
            definition(
                &image,
                RAM_FS_RECORD,
                ComponentSpec {
                    name: CYCLE_NAMES[1],
                    after: &CYCLE_TAIL_EDGE,
                    ..ram_filesystem()
                },
            ),
        ],
    );

    kernel_tests::kassert_eq!(refused, Err(BootFailure::PlanOrder));
    kernel_tests::kassert!(system.instance(CYCLE_NAMES[0]).is_none());
    kernel_tests::kassert!(system.instance(CYCLE_NAMES[1]).is_none());
    kernel_tests::kassert_eq!(system.start_order(), &[0][..]);
    kernel_tests::kassert!(alive(&system, 0));
    system.shutdown();
}

#[kernel_test]
fn a_transaction_with_an_unresolved_edge_raises_nothing() {
    let image = image();
    let bytes = encode_plan(&[ram_filesystem()]).expect("scenario plan encodes");
    let mut system = started(&image, &bytes);

    let refused = system.extend(
        caller(),
        vec![definition(
            &image,
            RAM_FS_RECORD,
            ComponentSpec {
                name: ORPHAN_NAME,
                slots: ORPHAN_SLOTS,
                ..ram_filesystem()
            },
        )],
    );

    kernel_tests::kassert_eq!(refused, Err(BootFailure::Provider));
    kernel_tests::kassert!(system.instance(ORPHAN_NAME).is_none());
    kernel_tests::kassert!(alive(&system, 0));
    system.shutdown();
}

#[kernel_test]
fn a_transaction_past_the_supervisor_budget_raises_nothing() {
    let image = image();
    let bytes = encode_plan(&[ram_filesystem()]).expect("scenario plan encodes");
    let mut system = started(&image, &bytes);

    // Резерв компонента режется из ресурса супервизора, и предел набора упирается в него.
    let refused = system.extend(
        caller(),
        vec![definition(
            &image,
            RAM_FS_RECORD,
            ComponentSpec {
                name: GREEDY_NAME,
                memory_pages: GREEDY_MEMORY_PAGES,
                ..ram_filesystem()
            },
        )],
    );

    kernel_tests::kassert_eq!(refused, Err(BootFailure::Budget(BudgetFailure::Reserve)));
    kernel_tests::kassert!(system.instance(GREEDY_NAME).is_none());
    kernel_tests::kassert!(alive(&system, 0));
    system.shutdown();
}

#[kernel_test]
fn a_transaction_raises_a_device_component_the_plan_never_named() {
    let image = image();
    // План не называет устройств, и supervisor берёт полномочия провижининга под набор.
    let bytes = encode_plan(&[ram_filesystem()]).expect("scenario plan encodes");
    let mut system = started(&image, &bytes);
    let device = rtc_device(&[RTC_COMPATIBLE]);

    system
        .extend(caller(), vec![definition(&image, RTC_RECORD, rtc(&device))])
        .expect("transaction raises the driver");

    let session = system
        .caller_session(caller(), RTC_NAME, RTC_PROTOCOL)
        .expect("rtc session minted");
    let clock = rtc::Client::new(PortTransport::client(&session)).with_wait(RTC_CALL_TIMEOUT);
    kernel_tests::kassert!(clock.now().is_ok());

    drop(session);
    // Завершение процесса драйвера освобождает линию устройства.
    system.shutdown();
}

#[kernel_test]
fn a_session_reaches_only_the_components_its_caller_defined() {
    let image = image();
    let bytes = encode_plan(&[ram_filesystem()]).expect("scenario plan encodes");
    let mut system = started(&image, &bytes);

    system
        .extend(
            caller(),
            vec![definition(&image, RAM_FS_RECORD, defined_ramfs())],
        )
        .expect("transaction raises the component");

    kernel_tests::kassert!(
        system
            .caller_session(caller(), DEFINED_NAME, FILE_SYSTEM_PROTOCOL)
            .is_ok()
    );
    // Значок вызывателя - единственное правило допуска. Набор достаётся записи, которая определила
    // его, а статическое связывание раздаёт компоненты плана.
    kernel_tests::kassert!(
        system
            .caller_session(other_caller(), DEFINED_NAME, FILE_SYSTEM_PROTOCOL)
            .is_err()
    );
    kernel_tests::kassert!(
        system
            .caller_session(caller(), RAM_FS_NAME, FILE_SYSTEM_PROTOCOL)
            .is_err()
    );

    system.shutdown();
}

#[kernel_test]
fn a_transaction_of_a_badge_outside_the_composition_raises_nothing() {
    let image = image();
    let bytes = encode_plan(&[ram_filesystem()]).expect("scenario plan encodes");
    let mut system = started(&image, &bytes);

    let refused = system.extend(
        caller_outside_the_composition(),
        vec![definition(&image, RAM_FS_RECORD, defined_ramfs())],
    );
    kernel_tests::kassert_eq!(refused, Err(BootFailure::Caller));
    // Значок супервизора стоит вне нумерации записей плана.
    let refused = system.extend(
        relay::SUPERVISOR_BADGE,
        vec![definition(&image, RAM_FS_RECORD, defined_ramfs())],
    );
    kernel_tests::kassert_eq!(refused, Err(BootFailure::Caller));
    kernel_tests::kassert!(system.component(1).is_none());

    // Обслуживание сохраняется при отказе, поэтому следующая транзакция идёт обычным порядком.
    system
        .extend(
            caller(),
            vec![definition(&image, RAM_FS_RECORD, defined_ramfs())],
        )
        .expect("transaction raises the component");
    kernel_tests::kassert!(
        system
            .caller_session(caller(), DEFINED_NAME, FILE_SYSTEM_PROTOCOL)
            .is_ok()
    );
    kernel_tests::kassert!(
        system
            .caller_session(
                caller_outside_the_composition(),
                DEFINED_NAME,
                FILE_SYSTEM_PROTOCOL,
            )
            .is_err()
    );

    system.shutdown();
}

#[kernel_test]
fn a_component_badge_names_the_record_it_was_minted_from() {
    kernel_tests::kassert!(relay::component_index(relay::SUPERVISOR_BADGE).is_none());
    for index in [0, 1, u16::MAX] {
        kernel_tests::kassert_eq!(
            relay::component_index(relay::component_badge(index)),
            Some(index)
        );
    }
    let past_records = NonZeroU64::new(u64::from(u16::MAX) + 3).expect("badge is non-zero");
    kernel_tests::kassert!(relay::component_index(past_records).is_none());
}

#[kernel_test]
fn an_optional_component_leaves_the_composition_and_a_critical_one_stops_it() {
    let image = image();
    let device = rtc_device(&[RTC_COMPATIBLE]);
    // Драйвер стоит в плане первым, а поднимается вторым, потому что `after` задаёт порядок.
    let bytes = encode_plan(&[
        ComponentSpec {
            after: &[ROUTER_NAME],
            ..rtc(&device)
        },
        router(OnExhausted::Halt, 1),
        image_filesystem(),
        ram_filesystem(),
    ])
    .expect("scenario plan encodes");
    let mut system = started(&image, &bytes);
    kernel_tests::kassert_eq!(system.start_order(), &[1, 2, 3, 0][..]);

    kernel_tests::kassert!(terminate(&system, 0, DEVICE_EXIT_CODE));
    let abandoned = system.supervise_turn(relay_thread());

    // Некритический компонент снят с композиции, а критический остался жив.
    kernel_tests::kassert_eq!(
        abandoned,
        Supervised::Abandoned {
            exit_code: Some(DEVICE_EXIT_CODE),
            dependents: 0,
        }
    );
    kernel_tests::kassert!(managed(&system, 0).running().is_none());
    kernel_tests::kassert!(alive(&system, 1));

    kernel_tests::kassert!(terminate(&system, 1, ROUTER_EXIT_CODE));
    let stopped = system.supervise_turn(relay_thread());

    // Уход критического компонента останавливает всю композицию.
    kernel_tests::kassert_eq!(stopped, Supervised::Stopped(Stopped::Subsystem));
}

#[kernel_test]
fn compiled_stage_one_carries_one_component_per_record() {
    let image = image();
    let entries = image.entries().expect("image decodes");

    let record = entries
        .entry(STAGE_ONE_RECORD)
        .expect("image carries the stage 1 record");
    let plan = decode_plan(record.data()).expect("stage 1 record decodes");

    // `define` принимает запись плана ровно с одним компонентом, и сборка кодирует её такой,
    // поэтому владелец политики загрузки не перекодирует записи.
    kernel_tests::kassert_eq!(plan.len(), 1);
    let ramfs = plan.component(0).expect("record carries its component");
    kernel_tests::kassert_eq!(ramfs.executable(), RAM_FS_PROGRAM);
    kernel_tests::kassert_eq!(ramfs.name(), RAM_FS_NAME);
    kernel_tests::kassert!(ramfs.establishes_sessions());
}

#[kernel_test]
fn init_raises_stage_one_from_the_image_and_mounts_its_file_system() {
    let image = image();
    let bytes = boot_plan();
    let mut system = started(&image, &bytes);
    // Ступень 0 содержит один корень, потому что в композиции плана нет файловой системы `/tmp`.
    kernel_tests::kassert!(system.instance(RAM_FS_NAME).is_none());

    raise_stage_one(&mut system, relay_thread());

    let raised = system
        .instance(RAM_FS_NAME)
        .expect("the transaction added the stage 1 file system");
    kernel_tests::kassert!(alive(&system, raised));
    // `init` взял сессию поднятого компонента у супервизора и сам присоединил её, потому что у него
    // нет статического слота под этот префикс.
    let temp = await_tree(&system, RAM_MOUNT);
    kernel_tests::kassert!(matches!(probe(&temp), Ok(Ok(_))));

    system.shutdown();
}

#[kernel_test]
fn a_stage_one_component_logs_under_its_instance_name() {
    let image = image();
    let bytes = boot_plan();
    crate::log_relay::start_component_relay();
    let mut system = RootSystem::bring_up(
        &image,
        plan(&bytes),
        crate::bootstrap_handle(),
        crate::log_relay::component_scenario_session,
    )
    .expect("composition started");

    raise_stage_one(&mut system, scenario_relay_thread());

    kernel_tests::kassert!(system.instance(RAM_FS_NAME).is_some());
    // Supervisor ставит имена источников по набору композиции, а не по одному плану, поэтому
    // компонент транзакции пишет под своим именем, а не под `badge-<hex>`.
    let sources = crate::log_relay::await_component_sources(&[RAM_FS_NAME]);
    kernel_tests::kassert!(
        sources
            .iter()
            .any(|source| source.split('/').next() == Some(RAM_FS_NAME))
    );
    kernel_tests::kassert!(!sources.iter().any(|source| source.starts_with("badge-")));

    system.shutdown();
    crate::log_relay::stop_component_relay();
}

#[kernel_test]
fn a_transaction_raises_a_component_taking_its_directory_from_the_composition() {
    let image = image();
    let bytes = boot_plan();
    let mut system = started(&image, &bytes);

    system
        .extend(
            caller(),
            vec![definition(&image, AREA_RECORD, area_consumer())],
        )
        .expect("transaction raises the directory consumer");

    // Компонент читает свою запись сессией каталога. Он уносит прочие исходы своим кодом выхода.
    let read = await_turn(&mut system);
    kernel_tests::kassert_eq!(
        read,
        Supervised::Restarted {
            exit_code: Some(AREA_READ_EXIT_CODE),
            dependents: 0,
        }
    );

    system.shutdown();
}
