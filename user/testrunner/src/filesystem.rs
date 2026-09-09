//! E2E файловой подсистемы: rootkeeper поднимает файловые системы, политика загрузки монтирует их
//! сессии маршрутизатору, а он владеет таблицей монтирования.

use alloc::{boxed::Box, format, string::String, vec, vec::Vec};

use bootstrap_abi::bootstrap;
use component_abi::{SESSION_SCOPE_MAX, component_startup};
use filesystem_abi::{
    APPEND_OFFSET, AccessMode, DirCursor, DirEntry, FileSystemError, NAME_MAX, NodeDescriptor,
    NodeKind, OpenFlags, Opened, Path, file_system, namespace, namespace_admin,
};
use ipc::{
    MessageLen, Transport,
    wire::{HEADER_SIZE, Header, IpcError, MESSAGE_INLINE_MAX, Str, TERMINATOR},
};
use kernel_objects::{
    BorrowedHandle, MemoryRegion, OwnedHandle, Port, Process, Resource, Signal, kind,
};
use kernel_tests::kernel_test;
use managed_component::{ComponentRoots, RunningComponent};
use rootkeeper::{RootSystem, SystemImage};
use runtime::{PortTransport, Timeout, UserMemFlags, port_send};
use spin::Once;
use syscall::Rights;
use userland::USERLAND_PAGE_SIZE;
use userland_image::{component_name, decode};
use userland_loader::load_entry;

pub(crate) const CALL_TIMEOUT: Timeout = Timeout::from_secs(2);

const CLIENT_TIMEOUT: Timeout = Timeout::from_secs(2);

const CLIENT_FIXTURE: &str = "/boot/filesystem-client-fixture/program";

const RAM_MOUNT: &str = "/tmp";
const SPARE_MOUNT: &str = "/spare";

const ROUTER: u16 = 0;

const NAMESPACE_EXPORT: usize = 0;
const NAMESPACE_ADMIN_EXPORT: usize = 1;

const SINGLE_EXPORT: u16 = 0;
const UNRESPONSIVE_FILESYSTEM: u16 = 2;
const SPARE_FILESYSTEM: u16 = 3;

const RAM_FILESYSTEM_NAME: &str = "filesystem-ramfs";

const BOOT_TREE_ATTEMPTS: usize = 200;
const BOOT_TREE_TURN: Timeout = Timeout::from_millis(10);

pub(crate) const BUFFER_SIZE: u32 = 4096;

const LISTING_ENTRIES: usize = 70;

pub(crate) const IMAGE_DIRECTORY: &str = "/boot";

pub(crate) const COMPONENT_DIRECTORY: &str = "/boot/testrunner";

pub(crate) const IMAGE_FILE: &str = "/boot/testrunner/sample";
pub(crate) const IMAGE_FILE_BODY: &[u8] = b"hello from the userland image\n";

const IMAGE_EMPTY_FILE: &str = "/boot/testrunner/empty";

const ABANDONED_PATH: &str = "/abandoned";

const LEAK_FRAMES: usize = syscall::HANDLE_TABLE_CAPACITY as usize + 128;

const REAP_SESSIONS: usize = 40;

const REAP_BUFFER_SIZE: u32 = 64 * 1024;

struct NamespacePorts {
    system: RootSystem<'static>,
}

static NAMESPACE_PORTS: Once<NamespacePorts> = Once::new();

fn router_minter() -> BorrowedHandle<'static, kind::Port> {
    export_minter(component_roots(ROUTER), NAMESPACE_EXPORT)
}

fn router_admin() -> BorrowedHandle<'static, kind::Port> {
    export_minter(component_roots(ROUTER), NAMESPACE_ADMIN_EXPORT)
}

fn export_minter(roots: &ComponentRoots, export: usize) -> BorrowedHandle<'_, kind::Port> {
    roots
        .export_minter(export)
        .expect("component exports the protocol")
}

fn namespace_ports() -> &'static NamespacePorts {
    NAMESPACE_PORTS.call_once(start_namespace)
}

fn component_roots(index: u16) -> &'static ComponentRoots {
    component_running(index).roots()
}

fn component_running(index: u16) -> &'static RunningComponent<ComponentRoots> {
    running_of(namespace_ports(), index)
}

fn ram_filesystem() -> u16 {
    namespace_ports()
        .system
        .instance(RAM_FILESYSTEM_NAME)
        .expect("stage 1 raised the file system of /tmp")
}

fn running_of(ports: &NamespacePorts, index: u16) -> &RunningComponent<ComponentRoots> {
    ports
        .system
        .component(index)
        .expect("composition carries the component")
        .managed()
        .running()
        .expect("component is running")
}

pub(crate) fn whole_export() -> Str<'static, SESSION_SCOPE_MAX> {
    Str::new("").expect("the empty scope fits the bound")
}

fn filesystem_session(index: u16) -> Port {
    filesystem_session_at(index, whole_export())
}

fn filesystem_session_at(index: u16, scope: Str<'_, SESSION_SCOPE_MAX>) -> Port {
    let mut response =
        component_startup::Client::new(PortTransport::client(component_running(index).startup()))
            .with_wait(CALL_TIMEOUT)
            .open_session(SINGLE_EXPORT, scope)
            .expect("open_session call");
    let capability = response.value.expect("session established");
    let session = response.claim(capability).expect("capability owner");
    Port::from(session)
}

fn session_scope(text: &str) -> Str<'_, SESSION_SCOPE_MAX> {
    Str::new(text).expect("scope within bound")
}

fn namespace_admin_client(
    capability: BorrowedHandle<'_, kind::Port>,
) -> namespace_admin::Client<PortTransport> {
    namespace_admin::Client::new(PortTransport::client(capability)).with_wait(CALL_TIMEOUT)
}

fn start_namespace() -> NamespacePorts {
    let image = Box::leak(Box::new(
        SystemImage::acquire(crate::bootstrap_handle()).expect("userland image is mapped"),
    ));
    let plan = image
        .plan(crate::rootkeeper_system::BOOT_PLAN_ENTRY)
        .expect("compiled boot plan decodes");
    let mut system = RootSystem::bring_up(
        image,
        plan,
        crate::bootstrap_handle(),
        crate::log_relay::component_log_session,
    )
    .expect("filesystem composition started");
    // Ступень 1 подаёт `init` транзакцией, и оборот композиции обслуживает её операции. Сюита
    // держит композицию статиком и не делает своих оборотов.
    let relay = crate::log_relay::shared_relay_thread().expect("shared relay is running");
    crate::rootkeeper_system::raise_stage_one(&mut system, relay);
    let ports = NamespacePorts { system };
    await_boot_tree(&ports);
    ports
}

fn await_boot_tree(ports: &NamespacePorts) {
    // Компонент композиции монтирует дерево имён загрузки своим оборотом, и его префиксы становятся
    // накрытыми не раньше.
    for _ in 0..BOOT_TREE_ATTEMPTS {
        let minter = export_minter(running_of(ports, ROUTER).roots(), NAMESPACE_EXPORT);
        if [Path::ROOT.as_str(), RAM_MOUNT]
            .into_iter()
            .all(|prefix| covers(minter, prefix))
        {
            return;
        }
        crate::rootkeeper_system::pause(BOOT_TREE_TURN);
    }
    panic!("boot tree is not mounted")
}

fn covers(minter: BorrowedHandle<'_, kind::Port>, prefix: &str) -> bool {
    // Выданную сессию закрывает ответ: проверке довольно покрытия префикса.
    let Ok(resolved) = namespace_client(minter).resolve(path(prefix)) else {
        return false;
    };
    resolved.value.is_ok()
}

fn spawn_from_image(path: &str, start: Port) -> Process {
    let bootstrap = bootstrap::Client::new(PortTransport::client(crate::bootstrap_handle()));
    let region = acquire_image(&bootstrap);
    let mapping = region.map_full_read_only().expect("map userland image");
    let image = decode(mapping.as_bytes()).expect("decode userland image");
    let program = image
        .entry(path)
        .expect("image carries the entry")
        .program()
        .expect("entry is a program");
    let resource = Resource::self_resource().expect("self metering resource");
    load_entry(&program, Some(component_name(path)), &resource, start).expect("spawn from image")
}

fn acquire_image(bootstrap: &bootstrap::Client<PortTransport>) -> MemoryRegion {
    let mut response = bootstrap
        .acquire_userland_image()
        .expect("acquire_userland_image call");
    let capability = response.value.expect("image vend");
    MemoryRegion::from(response.claim(capability).expect("capability owner"))
}

fn namespace_client(
    capability: BorrowedHandle<'_, kind::Port>,
) -> namespace::Client<PortTransport> {
    namespace::Client::new(PortTransport::client(capability)).with_wait(CALL_TIMEOUT)
}

pub(crate) fn mount_ram_filesystem(prefix: &str) -> Result<u32, FileSystemError> {
    mount_filesystem(prefix, ram_filesystem())
}

pub(crate) fn mount_ram_filesystem_scoped(
    prefix: &str,
    scope: &str,
) -> Result<u32, FileSystemError> {
    let index = ram_filesystem();
    // Окно открывается по существующему узлу. Каталог создаётся сессией на весь экспорт.
    create_directory(index, scope);
    namespace_admin_client(router_admin())
        .mount(
            path(prefix),
            filesystem_session_at(index, session_scope(scope)),
        )
        .expect("mount call")
        .value
}

pub(crate) fn unmount_ram_filesystem_scoped(
    prefix: &str,
    scope: &str,
) -> Result<u32, FileSystemError> {
    let index = ram_filesystem();
    let unmounted = unmount_filesystem(prefix)?;
    // Корень окна лежит записью в каталоге над ним. Сессия на весь экспорт снимает её.
    remove_directory(index, scope);
    Ok(unmounted)
}

fn create_directory(index: u16, text: &str) {
    let capability = filesystem_session(index);
    let client = connect(&capability);
    let opened = open(
        &client,
        text,
        AccessMode::Read,
        OpenFlags::CREATE | OpenFlags::DIRECTORY,
    );
    close(&client, &opened);
}

fn remove_directory(index: u16, text: &str) {
    let capability = filesystem_session(index);
    let client = connect(&capability);
    client
        .unlink(path(text), NodeKind::Directory)
        .expect("unlink call")
        .value
        .expect("unlink of an emptied directory");
}

fn mount_filesystem(prefix: &str, index: u16) -> Result<u32, FileSystemError> {
    namespace_admin_client(router_admin())
        .mount(path(prefix), filesystem_session(index))
        .expect("mount call")
        .value
}

pub(crate) fn unmount_filesystem(prefix: &str) -> Result<u32, FileSystemError> {
    namespace_admin_client(router_admin())
        .unmount(path(prefix))
        .expect("unmount call")
        .value
}

pub(crate) fn session_for(text: &str) -> Port {
    let mut response = namespace_client(router_minter())
        .resolve(path(text))
        .expect("resolve call");
    let capability = response.value.expect("path is covered by a mount");
    let session = response.claim(capability).expect("capability owner");
    Port::from(session)
}

pub(crate) fn ram_session() -> Port {
    session_for(RAM_MOUNT)
}

pub(crate) fn image_session() -> Port {
    session_for(Path::ROOT.as_str())
}

pub(crate) fn connect(capability: &Port) -> file_system::Client<PortTransport> {
    file_system::Client::new(PortTransport::client(capability)).with_wait(CALL_TIMEOUT)
}

#[derive(Clone, Copy)]
struct Tamper {
    one_way: bool,
    unknown_ordinal: bool,
    empty_body: bool,
}

struct Tampered {
    port: PortTransport,
    tamper: Tamper,
}

impl Tamper {
    const NONE: Self = Self {
        one_way: false,
        unknown_ordinal: false,
        empty_body: false,
    };

    fn apply(self, bytes: &[u8], out: &mut [u8]) -> Result<usize, IpcError> {
        let mut header = Header::decode(bytes)?;
        if self.one_way {
            header.txid = 0;
        }
        if self.unknown_ordinal {
            header.ordinal ^= 1;
        }
        out[..bytes.len()].copy_from_slice(bytes);
        header.encode(out)?;
        if self.empty_body {
            out[HEADER_SIZE] = TERMINATOR;
            return Ok(HEADER_SIZE + 1);
        }
        Ok(bytes.len())
    }
}

impl Transport for Tampered {
    type Resources = <PortTransport as Transport>::Resources;

    fn take_resources(&self) -> Self::Resources {
        self.port.take_resources()
    }

    fn write_message(&self, bytes: &[u8], handles: &[u32]) -> Result<(), IpcError> {
        let mut frame = [0u8; MESSAGE_INLINE_MAX];
        let len = self.tamper.apply(bytes, &mut frame)?;
        self.port.write_message(&frame[..len], handles)
    }

    fn read_message(&self, bytes: &mut [u8], handles: &mut [u32]) -> Result<MessageLen, IpcError> {
        self.port.read_message(bytes, handles)
    }

    fn wait_readable(&self, timeout: Timeout) -> Result<(), IpcError> {
        self.port.wait_readable(timeout)
    }
}

fn connect_tampered(capability: &Port, tamper: Tamper) -> file_system::Client<Tampered> {
    file_system::Client::new(Tampered {
        port: PortTransport::client(capability),
        tamper,
    })
    .with_wait(CALL_TIMEOUT)
}

fn connect_abandoning(capability: &Port) -> file_system::Client<Tampered> {
    connect_tampered(
        capability,
        Tamper {
            one_way: true,
            ..Tamper::NONE
        },
    )
}

pub(crate) fn acquire_buffer(client: &file_system::Client<PortTransport>) -> MemoryRegion {
    let mut response = client
        .acquire_buffer(BUFFER_SIZE)
        .expect("acquire_buffer call");
    let capability = response.value.expect("buffer granted");
    let cap = response.claim(capability).expect("capability owner");
    MemoryRegion::from(cap)
}

pub(crate) fn path(text: &str) -> Str<'_, { filesystem_abi::PATH_MAX }> {
    Str::new(text).expect("path within bound")
}

pub(crate) fn open(
    client: &file_system::Client<PortTransport>,
    text: &str,
    mode: AccessMode,
    flags: OpenFlags,
) -> Opened {
    client
        .open(path(text), mode, flags)
        .expect("open call")
        .value
        .expect("open ok")
}

fn close(client: &file_system::Client<PortTransport>, opened: &Opened) {
    client
        .close(opened.fd)
        .expect("close call")
        .value
        .expect("close ok");
}

pub(crate) fn write_all(
    client: &file_system::Client<PortTransport>,
    buffer: &kernel_objects::Mapping,
    opened: &Opened,
    offset: u64,
    payload: &[u8],
) -> u32 {
    buffer.copy_in(payload).expect("payload into the buffer");
    let written = client
        .write(opened.fd, offset, payload.len() as u32)
        .expect("write call")
        .value
        .expect("write ok");
    written.count
}

pub(crate) fn buffer_head(buffer: &kernel_objects::Mapping, len: usize) -> Vec<u8> {
    let mut bytes = vec![0; len];
    buffer.copy_out(&mut bytes).expect("buffer head");
    bytes
}

fn buffer_bytes(buffer: &kernel_objects::Mapping) -> Vec<u8> {
    buffer_head(
        buffer,
        usize::try_from(buffer.size_bytes()).expect("buffer size fits usize"),
    )
}

fn collect_entries(buffer: &[u8], count: u32, names: &mut Vec<String>) {
    let mut offset = 0;
    for _ in 0..count {
        let decoded = DirEntry::decode(&buffer[offset..]).expect("directory record");
        names.push(String::from(decoded.entry.name));
        offset += decoded.len;
    }
}

fn entry_name(index: usize) -> String {
    format!("{index:0>NAME_MAX$}")
}

pub(crate) fn walk_directory(
    client: &file_system::Client<PortTransport>,
    buffer: &kernel_objects::Mapping,
    fd: NodeDescriptor,
) -> Vec<String> {
    let mut names = Vec::new();
    let mut from = DirCursor::START;
    loop {
        let listed = client
            .list(fd, from)
            .expect("list call")
            .value
            .expect("list ok");
        collect_entries(&buffer_bytes(buffer), listed.count, &mut names);
        if listed.end {
            return names;
        }
        // Перечисление обязано продвинуться, иначе записи не помещаются в буфер и повтор вызова не
        // изменит ничего.
        kernel_tests::kassert!(listed.next.raw() > from.raw());
        from = listed.next;
    }
}

#[kernel_test]
fn filesystem_creates_and_writes_a_file() {
    let capability = ram_session();
    let client = connect(&capability);
    let region = acquire_buffer(&client);
    let buffer = region
        .map_full(UserMemFlags::ReadWrite)
        .expect("map data buffer");

    let opened = open(
        &client,
        "/created",
        AccessMode::ReadWrite,
        OpenFlags::CREATE,
    );
    kernel_tests::kassert_eq!(opened.kind, NodeKind::File);
    kernel_tests::kassert_eq!(opened.size, 0);

    let payload = b"hello, file system";
    let count = write_all(&client, &buffer, &opened, 0, payload);
    kernel_tests::kassert_eq!(count, payload.len() as u32);

    let info = client
        .stat(opened.fd)
        .expect("stat call")
        .value
        .expect("stat ok");
    kernel_tests::kassert_eq!(info.size, payload.len() as u64);

    let again = client
        .open(
            path("/created"),
            AccessMode::Read,
            OpenFlags::CREATE | OpenFlags::EXCLUSIVE,
        )
        .expect("open call")
        .value;
    kernel_tests::kassert_eq!(again, Err(FileSystemError::AlreadyExists));
}

#[kernel_test]
fn filesystem_reads_at_offset() {
    let capability = ram_session();
    let client = connect(&capability);
    let region = acquire_buffer(&client);
    let buffer = region
        .map_full(UserMemFlags::ReadWrite)
        .expect("map data buffer");

    let opened = open(&client, "/digits", AccessMode::ReadWrite, OpenFlags::CREATE);
    let payload = b"0123456789";
    write_all(&client, &buffer, &opened, 0, payload);
    buffer
        .copy_in(&vec![0; payload.len()])
        .expect("clear the buffer");

    let count = client
        .read(opened.fd, 4, 6)
        .expect("read call")
        .value
        .expect("read ok");
    kernel_tests::kassert_eq!(count, 6);
    kernel_tests::kassert_eq!(buffer_head(&buffer, 6), b"456789");

    let tail = client
        .read(opened.fd, payload.len() as u64, 4)
        .expect("read call")
        .value
        .expect("read ok");
    kernel_tests::kassert_eq!(tail, 0);
}

#[kernel_test]
fn filesystem_stats_file_and_directory() {
    let capability = ram_session();
    let client = connect(&capability);
    let region = acquire_buffer(&client);
    let buffer = region
        .map_full(UserMemFlags::ReadWrite)
        .expect("map data buffer");

    let directory = open(
        &client,
        "/stat-dir",
        AccessMode::Read,
        OpenFlags::CREATE | OpenFlags::DIRECTORY,
    );
    kernel_tests::kassert_eq!(directory.kind, NodeKind::Directory);
    kernel_tests::kassert_eq!(directory.size, 0);

    let file = open(
        &client,
        "/stat-dir/entry",
        AccessMode::ReadWrite,
        OpenFlags::CREATE,
    );
    write_all(&client, &buffer, &file, 0, b"stat");

    let file_info = client
        .stat(file.fd)
        .expect("stat call")
        .value
        .expect("stat ok");
    kernel_tests::kassert_eq!(file_info.kind, NodeKind::File);
    kernel_tests::kassert_eq!(file_info.size, 4);

    let directory_info = client
        .stat(directory.fd)
        .expect("stat call")
        .value
        .expect("stat ok");
    kernel_tests::kassert_eq!(directory_info.kind, NodeKind::Directory);
    kernel_tests::kassert_eq!(directory_info.size, 1);
}

#[kernel_test]
fn filesystem_lists_directory_across_batches() {
    let capability = ram_session();
    let client = connect(&capability);
    let region = acquire_buffer(&client);
    let buffer = region
        .map_full(UserMemFlags::ReadWrite)
        .expect("map data buffer");

    let directory = open(
        &client,
        "/listing",
        AccessMode::Read,
        OpenFlags::CREATE | OpenFlags::DIRECTORY,
    );
    for index in 0..LISTING_ENTRIES {
        let entry = open(
            &client,
            &format!("/listing/{}", entry_name(index)),
            AccessMode::ReadWrite,
            OpenFlags::CREATE,
        );
        close(&client, &entry);
    }

    let mut names: Vec<String> = Vec::new();
    let mut batches = 0;
    let mut from = DirCursor::START;
    loop {
        let listed = client
            .list(directory.fd, from)
            .expect("list call")
            .value
            .expect("list ok");
        batches += 1;
        collect_entries(&buffer_bytes(&buffer), listed.count, &mut names);
        if listed.end {
            break;
        }
        // Перечисление обязано продвинуться, иначе записи не помещаются в буфер и повтор вызова не
        // изменит ничего.
        kernel_tests::kassert!(listed.count > 0 && listed.next.raw() > from.raw());
        from = listed.next;
    }

    kernel_tests::kassert!(batches >= 2);
    names.sort_unstable();
    names.dedup();
    kernel_tests::kassert_eq!(names.len(), LISTING_ENTRIES);
}

#[kernel_test]
fn filesystem_unlinks_a_file() {
    let capability = ram_session();
    let client = connect(&capability);

    let opened = open(
        &client,
        "/removed",
        AccessMode::ReadWrite,
        OpenFlags::CREATE,
    );
    close(&client, &opened);
    client
        .unlink(path("/removed"), NodeKind::File)
        .expect("unlink call")
        .value
        .expect("unlink ok");

    let reopened = client
        .open(path("/removed"), AccessMode::Read, OpenFlags::NONE)
        .expect("open call")
        .value;
    kernel_tests::kassert_eq!(reopened, Err(FileSystemError::NotFound));
    let again = client
        .unlink(path("/removed"), NodeKind::File)
        .expect("unlink call")
        .value;
    kernel_tests::kassert_eq!(again, Err(FileSystemError::NotFound));
}

#[kernel_test]
fn filesystem_rejects_write_through_a_read_only_descriptor() {
    let capability = ram_session();
    let client = connect(&capability);
    let region = acquire_buffer(&client);
    let buffer = region
        .map_full(UserMemFlags::ReadWrite)
        .expect("map data buffer");

    let opened = open(&client, "/read-only", AccessMode::Read, OpenFlags::CREATE);
    let payload = b"deny";
    buffer.copy_in(payload).expect("payload into the buffer");
    let written = client
        .write(opened.fd, 0, payload.len() as u32)
        .expect("write call")
        .value;
    kernel_tests::kassert_eq!(written, Err(FileSystemError::WrongMode));

    let resized = client.set_size(opened.fd, 16).expect("set_size call").value;
    kernel_tests::kassert_eq!(resized, Err(FileSystemError::WrongMode));

    // Усечение меняет узел, поэтому без режима записи оно отвергается ещё на открытии.
    let truncated = client
        .open(path("/read-only"), AccessMode::Read, OpenFlags::TRUNCATE)
        .expect("open call")
        .value;
    kernel_tests::kassert_eq!(truncated, Err(FileSystemError::WrongMode));
}

#[kernel_test]
fn namespace_resolves_each_path_to_the_longest_prefix() {
    let namespace = namespace_client(router_minter());

    // Каталог образа существует только в корневой imagefs.
    let mut response = namespace
        .resolve(path(IMAGE_DIRECTORY))
        .expect("resolve call");
    let capability = response.value.expect("root mount covers the path");
    let image = response.claim(capability).expect("capability owner");
    drop(Port::from(image));

    let ram = ram_session();
    connect(&ram)
        .open(
            path("/inner"),
            AccessMode::ReadWrite,
            OpenFlags::CREATE | OpenFlags::DIRECTORY,
        )
        .expect("create directory call")
        .value
        .expect("directory created");

    // `/tmp/inner` отсутствует в imagefs, поэтому успешное окно доказывает выбор длинного префикса.
    let mut response = namespace.resolve(path("/tmp/inner")).expect("resolve call");
    let capability = response.value.expect("ram mount covers the path");
    let ram = response.claim(capability).expect("capability owner");
    drop(Port::from(ram));
}

#[kernel_test]
fn namespace_refuses_an_occupied_prefix_without_changing_the_table() {
    // Отвергнутое монтирование уносит сессию, которую ему передали, поэтому вернуть её отправителю
    // нечем.
    let occupied = mount_filesystem(RAM_MOUNT, ram_filesystem());
    kernel_tests::kassert_eq!(occupied, Err(FileSystemError::AlreadyExists));

    let mut response = namespace_client(router_minter())
        .resolve(path(RAM_MOUNT))
        .expect("resolve call");
    let capability = response.value.expect("original mount remains");
    let mounted = response.claim(capability).expect("capability owner");
    drop(Port::from(mounted));
}

#[kernel_test]
fn namespace_grafts_the_session_it_is_handed() {
    const GRAFT_MOUNT: &str = "/grafted";
    const GRAFT_ENTRY: &str = "/grafted-probe";

    // Происхождение капы маршрутизатору неизвестно, поэтому под префиксом встаёт тот сервер, чью
    // сессию ему передали.
    let handed = filesystem_session(ram_filesystem());
    open(
        &connect(&handed),
        GRAFT_ENTRY,
        AccessMode::ReadWrite,
        OpenFlags::CREATE,
    );
    namespace_admin_client(router_admin())
        .mount(path(GRAFT_MOUNT), handed)
        .expect("mount call")
        .value
        .expect("handed session grafted");

    let grafted = session_for(GRAFT_MOUNT);
    let opened = open(
        &connect(&grafted),
        GRAFT_ENTRY,
        AccessMode::Read,
        OpenFlags::NONE,
    );
    kernel_tests::kassert_eq!(opened.kind, NodeKind::File);

    // Переданная сессия создала узел, и корневое монтирование его не содержит.
    let absent = connect(&image_session())
        .open(path(GRAFT_ENTRY), AccessMode::Read, OpenFlags::NONE)
        .expect("open call")
        .value;
    kernel_tests::kassert_eq!(absent, Err(FileSystemError::NotFound));

    unmount_filesystem(GRAFT_MOUNT).expect("grafted mount removed");
}

#[kernel_test]
fn namespace_rejects_admin_operations_on_the_service_port() {
    // Управление таблицей монтирования находится на порту экспорта `NamespaceAdmin`, поэтому
    // порт `Namespace` его ordinal не разбирает, и кадр отвергается целиком, не доходя до
    // операции.
    let admin_on_service = namespace_admin_client(router_minter());
    let refused = admin_on_service
        .unmount(path("/"))
        .map(|response| response.value);
    kernel_tests::kassert_eq!(refused, Err(IpcError::PeerClosed));

    // Отвергнутый кадр не тронул ни таблицу, ни цикл обслуживания, поэтому корень резолвится
    // дальше.
    let mut response = namespace_client(router_minter())
        .resolve(path(IMAGE_FILE))
        .expect("resolve call");
    let capability = response
        .value
        .expect("root mount survives the rejected frame");
    let mounted = response.claim(capability).expect("capability owner");
    drop(Port::from(mounted));
}

#[kernel_test]
fn namespace_reports_an_unresponsive_mount() {
    const STUCK_MOUNT: &str = "/boot";

    mount_filesystem(STUCK_MOUNT, UNRESPONSIVE_FILESYSTEM).expect("fixture mounted");

    // Сессия остаётся действующей без обслуживающего потока, поэтому `open_root` не отвечает, и
    // маршрутизатор отпускает вызов по своему сроку.
    let resolved = namespace_client(router_minter())
        .resolve(path(STUCK_MOUNT))
        .expect("resolve call")
        .value;
    kernel_tests::kassert_eq!(resolved, Err(FileSystemError::Unresponsive));

    // Снятие отпускает зависшую сессию, и корневое монтирование снова накрывает путь.
    unmount_filesystem(STUCK_MOUNT).expect("fixture unmounted");
    let mut response = namespace_client(router_minter())
        .resolve(path(STUCK_MOUNT))
        .expect("resolve call");
    let capability = response.value.expect("root mount covers the path again");
    let rerouted = response.claim(capability).expect("capability owner");
    drop(Port::from(rerouted));
}

#[kernel_test]
fn namespace_resolves_a_pair_to_the_session_of_their_common_directory() {
    let session = ram_session();
    let ram = connect(&session);
    close(
        &ram,
        &open(
            &ram,
            "/pair-left",
            AccessMode::Read,
            OpenFlags::CREATE | OpenFlags::DIRECTORY,
        ),
    );
    close(
        &ram,
        &open(
            &ram,
            "/pair-left/first",
            AccessMode::ReadWrite,
            OpenFlags::CREATE,
        ),
    );

    let namespace = namespace_client(router_minter());
    let mut response = namespace
        .resolve_pair(path("/tmp/pair-left/first"), path("/tmp/pair-right/second"))
        .expect("resolve_pair call");
    let pair = response.value.expect("one mount covers the pair");
    // Общий каталог пары - `/tmp`, и его длина отсчитывается от корня дерева имён.
    kernel_tests::kassert_eq!(pair.root_len, RAM_MOUNT.len() as u32);
    let scope = Port::from(response.claim(pair.session).expect("capability owner"));

    let opened = open(
        &connect(&scope),
        "/pair-left/first",
        AccessMode::Read,
        OpenFlags::NONE,
    );
    kernel_tests::kassert_eq!(opened.kind, NodeKind::File);
}

#[kernel_test]
fn namespace_keeps_the_pair_scope_inside_the_mount_root() {
    // Общий каталог `/tmp` и `/tmp/inside` - корень дерева имён, который лежит выше монтирования.
    let pair = namespace_client(router_minter())
        .resolve_pair(path(RAM_MOUNT), path("/tmp/inside"))
        .expect("resolve_pair call")
        .value
        .expect("one mount covers the pair");
    kernel_tests::kassert_eq!(pair.root_len, RAM_MOUNT.len() as u32);
}

#[kernel_test]
fn namespace_reports_a_pair_under_different_mounts() {
    const SECOND_MOUNT: &str = "/paired-second";

    mount_ram_filesystem(SECOND_MOUNT).expect("second mount added");
    let namespace = namespace_client(router_minter());

    let across = namespace
        .resolve_pair(path("/tmp/first"), path("/paired-second/second"))
        .expect("resolve_pair call")
        .value
        .map(|pair| pair.root_len);
    kernel_tests::kassert_eq!(across, Err(FileSystemError::CrossMount));

    // Отказ пары не тронул таблицу, и каждая сторона по-прежнему разбирается поодиночке.
    let mut response = namespace
        .resolve(path("/paired-second/second"))
        .expect("resolve call");
    let capability = response.value.expect("second mount covers the path");
    drop(Port::from(
        response.claim(capability).expect("capability owner"),
    ));

    unmount_filesystem(SECOND_MOUNT).expect("second mount removed");
}

#[kernel_test]
fn namespace_closes_the_session_of_an_undelivered_pair_reply() {
    let abandoned = namespace::Client::new(Tampered {
        port: PortTransport::client(router_minter()),
        tamper: Tamper {
            one_way: true,
            ..Tamper::NONE
        },
    })
    .with_wait(CALL_TIMEOUT);
    for _ in 0..LEAK_FRAMES {
        // Сессию общего каталога маршрутизатор открывает до ответа, доставить который некому.
        let _ = abandoned.resolve_pair(path("/tmp/first"), path("/tmp/second"));
    }

    // Таблица маршрутизатора не исчерпана, поэтому недоставленный ответ каждый раз закрывал
    // выданную сессию.
    kernel_tests::kassert!(covers(router_minter(), RAM_MOUNT));
    // Провайдер снял эти сессии по уходу значка и по-прежнему обслуживает новую.
    let session = ram_session();
    let client = connect(&session);
    close(
        &client,
        &open(
            &client,
            "/undelivered-pair",
            AccessMode::ReadWrite,
            OpenFlags::CREATE,
        ),
    );
}

#[kernel_test]
fn namespace_pair_scope_survives_a_table_change() {
    const PAIRED_MOUNT: &str = "/paired-detached";

    mount_ram_filesystem(PAIRED_MOUNT).expect("fixture mounted");
    let mut response = namespace_client(router_minter())
        .resolve_pair(
            path("/paired-detached/first"),
            path("/paired-detached/second"),
        )
        .expect("resolve_pair call");
    let pair = response.value.expect("fixture covers the pair");
    let scope = Port::from(response.claim(pair.session).expect("capability owner"));

    unmount_filesystem(PAIRED_MOUNT).expect("fixture unmounted");

    // Выданная сессия держит решённый scope, и снятие записи её не трогает.
    let client = connect(&scope);
    close(
        &client,
        &open(&client, "/first", AccessMode::ReadWrite, OpenFlags::CREATE),
    );
    client
        .rename(path("/first"), path("/second"))
        .expect("rename call")
        .value
        .expect("rename inside the issued scope");
}

#[kernel_test]
fn unmount_only_removes_the_route_and_keeps_existing_sessions_alive() {
    const DETACHED_MOUNT: &str = "/boot";

    mount_ram_filesystem(DETACHED_MOUNT).expect("fixture mounted");
    let session = session_for(DETACHED_MOUNT);
    unmount_filesystem(DETACHED_MOUNT).expect("fixture unmounted");

    let opened = connect(&session)
        .open(path("/"), AccessMode::Read, OpenFlags::NONE)
        .expect("existing session still answers")
        .value
        .expect("existing session keeps its root");
    kernel_tests::kassert_eq!(opened.kind, NodeKind::Directory);
    let mut response = namespace_client(router_minter())
        .resolve(path(DETACHED_MOUNT))
        .expect("resolve call");
    let capability = response
        .value
        .expect("root mount covers the path after unmount");
    let rerouted = response.claim(capability).expect("capability owner");
    drop(Port::from(rerouted));
}

#[kernel_test]
fn filesystem_reads_a_file_from_the_image() {
    let capability = image_session();
    let client = connect(&capability);
    let region = acquire_buffer(&client);
    let buffer = region
        .map_full(UserMemFlags::ReadWrite)
        .expect("map data buffer");

    let opened = open(&client, IMAGE_FILE, AccessMode::Read, OpenFlags::NONE);
    kernel_tests::kassert_eq!(opened.kind, NodeKind::File);
    kernel_tests::kassert_eq!(opened.size, IMAGE_FILE_BODY.len() as u64);

    let count = client
        .read(opened.fd, 0, BUFFER_SIZE)
        .expect("read call")
        .value
        .expect("read ok");
    kernel_tests::kassert_eq!(count, IMAGE_FILE_BODY.len() as u32);
    kernel_tests::kassert_eq!(buffer_head(&buffer, count as usize), IMAGE_FILE_BODY);
}

#[kernel_test]
fn filesystem_lists_the_image_directory() {
    let capability = image_session();
    let client = connect(&capability);
    let region = acquire_buffer(&client);
    let buffer = region
        .map_full(UserMemFlags::ReadWrite)
        .expect("map data buffer");

    let directory = open(
        &client,
        IMAGE_DIRECTORY,
        AccessMode::Read,
        OpenFlags::DIRECTORY,
    );
    kernel_tests::kassert_eq!(directory.kind, NodeKind::Directory);

    let names = walk_directory(&client, &buffer, directory.fd);

    // Каждая программа образа лежит своим каталогом. Под `/boot` перечисляются они, а её записи -
    // внутри.
    for expected in ["testrunner", "namespace-router"] {
        kernel_tests::kassert!(names.iter().any(|name| name.as_str() == expected));
    }

    let own = open(
        &client,
        COMPONENT_DIRECTORY,
        AccessMode::Read,
        OpenFlags::DIRECTORY,
    );
    let names = walk_directory(&client, &buffer, own.fd);
    for expected in ["program", "sample", "empty"] {
        kernel_tests::kassert!(names.iter().any(|name| name.as_str() == expected));
    }
}

#[kernel_test]
fn filesystem_mounts_hold_separate_trees() {
    let ram_capability = ram_session();
    let ram = connect(&ram_capability);
    let image_capability = image_session();
    let image = connect(&image_capability);

    open(
        &ram,
        "/only-in-ram",
        AccessMode::ReadWrite,
        OpenFlags::CREATE,
    );

    // Разные процессы держат монтирования, поэтому узел одного в дереве другого не виден, а корень
    // каждого - корень его собственного монтирования.
    let absent = image
        .open(path("/only-in-ram"), AccessMode::Read, OpenFlags::NONE)
        .expect("open call")
        .value;
    kernel_tests::kassert_eq!(absent, Err(FileSystemError::NotFound));
    let foreign = ram
        .open(path(IMAGE_FILE), AccessMode::Read, OpenFlags::NONE)
        .expect("open call")
        .value;
    kernel_tests::kassert_eq!(foreign, Err(FileSystemError::NotFound));
}

#[kernel_test]
fn filesystem_hands_out_the_backing_window_of_an_image_file() {
    let capability = image_session();
    let client = connect(&capability);
    let region = acquire_buffer(&client);
    let buffer = region
        .map_full(UserMemFlags::ReadWrite)
        .expect("map data buffer");

    let opened = open(&client, IMAGE_FILE, AccessMode::Read, OpenFlags::NONE);
    let count = client
        .read(opened.fd, 0, BUFFER_SIZE)
        .expect("read call")
        .value
        .expect("read ok");

    let mut response = client
        .backing_memory(opened.fd)
        .expect("backing_memory call");
    let capability = response.value.expect("window granted");
    let window = MemoryRegion::from(response.claim(capability).expect("capability owner"));
    let mapped = window
        .map_full(UserMemFlags::ReadOnly)
        .expect("map backing window");
    // Маппинг округлён вверх до страницы, настоящую длину даёт `Opened.size`.
    kernel_tests::kassert!(mapped.size_bytes() >= opened.size);
    kernel_tests::kassert_eq!(
        buffer_head(&mapped, opened.size as usize),
        buffer_head(&buffer, count as usize)
    );

    // Дескриптор открыт на чтение, регион образа - read-only, поэтому капа не даёт писать в
    // страницы напрямую.
    kernel_tests::kassert!(window.map_full(UserMemFlags::ReadWrite).is_err());
}

#[kernel_test]
fn filesystem_serves_an_empty_file_from_the_image() {
    let capability = image_session();
    let client = connect(&capability);
    let _buffer = acquire_buffer(&client);

    let opened = open(&client, IMAGE_EMPTY_FILE, AccessMode::Read, OpenFlags::NONE);
    kernel_tests::kassert_eq!(opened.kind, NodeKind::File);
    kernel_tests::kassert_eq!(opened.size, 0);

    let info = client
        .stat(opened.fd)
        .expect("stat call")
        .value
        .expect("stat ok");
    kernel_tests::kassert_eq!(info.kind, NodeKind::File);
    kernel_tests::kassert_eq!(info.size, 0);

    let count = client
        .read(opened.fd, 0, BUFFER_SIZE)
        .expect("read call")
        .value
        .expect("read ok");
    kernel_tests::kassert_eq!(count, 0);

    // Файл без байт не занимает страниц образа, поэтому у него нет окна.
    let refused = client
        .backing_memory(opened.fd)
        .expect("backing_memory call")
        .value;
    kernel_tests::kassert_eq!(refused, Err(FileSystemError::NotSupported));
}

#[kernel_test]
fn filesystem_has_no_backing_window_for_an_empty_ramfs_file() {
    let capability = ram_session();
    let client = connect(&capability);

    let opened = open(
        &client,
        "/on-the-heap",
        AccessMode::ReadWrite,
        OpenFlags::CREATE,
    );
    let refused = client
        .backing_memory(opened.fd)
        .expect("backing_memory call")
        .value;
    kernel_tests::kassert_eq!(refused, Err(FileSystemError::NotSupported));
}

#[kernel_test]
fn filesystem_ramfs_backing_is_shared_and_pins_capacity() {
    let capability = ram_session();
    let client = connect(&capability);
    let data_region = acquire_buffer(&client);
    let data = data_region
        .map_full(UserMemFlags::ReadWrite)
        .expect("map data buffer");
    let opened = open(
        &client,
        "/region-backed",
        AccessMode::ReadWrite,
        OpenFlags::CREATE,
    );
    write_all(&client, &data, &opened, 0, b"before");

    let mut response = client
        .backing_memory(opened.fd)
        .expect("backing_memory call");
    let capability = response.value.expect("backing granted");
    let backing = MemoryRegion::from(response.claim(capability).expect("capability owner"));
    let mapped = backing
        .map_full(UserMemFlags::ReadWrite)
        .expect("map ramfs backing");
    mapped.copy_in(b"mapped").expect("write through mapping");
    let count = client
        .read(opened.fd, 0, 6)
        .expect("read call")
        .value
        .expect("read ok");
    kernel_tests::kassert_eq!(count, 6);
    kernel_tests::kassert_eq!(buffer_head(&data, 6), b"mapped");

    write_all(&client, &data, &opened, 0, b"server");
    kernel_tests::kassert_eq!(buffer_head(&mapped, 6), b"server");

    data.copy_in(b"x").expect("payload into data buffer");
    let beyond = client
        .write(opened.fd, mapped.size_bytes(), 1)
        .expect("write call")
        .value;
    kernel_tests::kassert_eq!(beyond, Err(FileSystemError::NoSpace));

    let growing = open(
        &client,
        "/heap-backed",
        AccessMode::ReadWrite,
        OpenFlags::CREATE,
    );
    let written = client
        .write(growing.fd, mapped.size_bytes(), 1)
        .expect("write call")
        .value
        .expect("unmapped file grows");
    kernel_tests::kassert_eq!(written.count, 1);
}

#[kernel_test]
fn filesystem_ramfs_zeroes_the_gap_of_a_sparse_write() {
    let capability = ram_session();
    let client = connect(&capability);
    let data_region = acquire_buffer(&client);
    let data = data_region
        .map_full(UserMemFlags::ReadWrite)
        .expect("map data buffer");
    let opened = open(&client, "/sparse", AccessMode::ReadWrite, OpenFlags::CREATE);
    write_all(&client, &data, &opened, 0, b"head");

    let mut response = client
        .backing_memory(opened.fd)
        .expect("backing_memory call");
    let capability = response.value.expect("backing granted");
    let backing = MemoryRegion::from(response.claim(capability).expect("capability owner"));
    let mapped = backing
        .map_full(UserMemFlags::ReadWrite)
        .expect("map ramfs backing");
    // Разрыв заполнен через маппинг. Запись за прежней длиной обязана вернуть туда нули.
    mapped.copy_in_at(4, b"stale").expect("stale bytes");

    data.copy_in(b"tail").expect("payload into data buffer");
    let written = client
        .write(opened.fd, 16, 4)
        .expect("write call")
        .value
        .expect("sparse write within capacity");
    kernel_tests::kassert_eq!(written.count, 4);

    let count = client
        .read(opened.fd, 0, 20)
        .expect("read call")
        .value
        .expect("read ok");
    kernel_tests::kassert_eq!(count, 20);
    kernel_tests::kassert_eq!(buffer_head(&data, 20), b"head\0\0\0\0\0\0\0\0\0\0\0\0tail");
}

#[kernel_test]
fn filesystem_ramfs_accepts_a_client_region_without_copying() {
    let creator_capability = ram_session();
    let creator = connect(&creator_capability);
    let resource = Resource::self_resource().expect("self resource");
    let region = resource
        .create_virtual(2 * USERLAND_PAGE_SIZE, runtime::MemoryAccess::RW)
        .expect("create shared region");
    let mapped = region
        .map_full(UserMemFlags::ReadWrite)
        .expect("map shared region");
    mapped.copy_in(b"client").expect("initialize region");
    creator
        .create_backed(path("/client-region"), region, 6)
        .expect("create_backed call")
        .value
        .expect("create backed file");

    let reader_capability = ram_session();
    let reader = connect(&reader_capability);
    let data_region = acquire_buffer(&reader);
    let data = data_region
        .map_full(UserMemFlags::ReadWrite)
        .expect("map data buffer");
    let opened = open(
        &reader,
        "/client-region",
        AccessMode::ReadWrite,
        OpenFlags::NONE,
    );
    let count = reader
        .read(opened.fd, 0, 6)
        .expect("read call")
        .value
        .expect("read ok");
    kernel_tests::kassert_eq!(count, 6);
    kernel_tests::kassert_eq!(buffer_head(&data, 6), b"client");

    mapped.copy_in(b"mapped").expect("change client mapping");
    let count = reader
        .read(opened.fd, 0, 6)
        .expect("read call")
        .value
        .expect("read ok");
    kernel_tests::kassert_eq!(count, 6);
    kernel_tests::kassert_eq!(buffer_head(&data, 6), b"mapped");

    write_all(&reader, &data, &opened, 0, b"server");
    kernel_tests::kassert_eq!(buffer_head(&mapped, 6), b"server");

    let mut response = reader
        .backing_memory(opened.fd)
        .expect("backing_memory call");
    let capability = response.value.expect("backing granted");
    let shared = MemoryRegion::from(response.claim(capability).expect("capability owner"));
    kernel_tests::kassert!(
        shared
            .duplicate(Rights::READ | Rights::TRANSFER, None)
            .is_err()
    );
}

fn create_filler_tree(
    client: &file_system::Client<PortTransport>,
    data: &kernel_objects::Mapping,
    names: &[String],
) {
    for name in names {
        let opened = open(client, name, AccessMode::ReadWrite, OpenFlags::CREATE);
        write_all(client, data, &opened, 0, b"f");
        close(client, &opened);
    }
}

fn occupy_mapping_slots(
    client: &file_system::Client<PortTransport>,
    names: &[String],
) -> Vec<String> {
    let mut occupied = Vec::new();
    for name in names {
        let opened = open(client, name, AccessMode::ReadWrite, OpenFlags::NONE);
        let backed = client
            .backing_memory(opened.fd)
            .expect("backing_memory call");
        close(client, &opened);
        // Ответ сам владеет выданным окном независимо от следующего вызова и закрывает его.
        match backed.value {
            Ok(_) => {
                occupied.push(name.clone());
            }
            Err(error) => {
                kernel_tests::kassert_eq!(error, FileSystemError::NoSpace);
                break;
            }
        }
    }
    occupied
}

fn buffered_sessions(mount: &str, count: usize) -> Vec<Port> {
    (0..count)
        .map(|_| {
            let capability = session_for(mount);
            let granted = connect(&capability)
                .acquire_buffer(BUFFER_SIZE)
                .expect("acquire_buffer call");
            // Выданный буфер закрывает ответ, а сессию держит её собственная capability.
            granted.value.expect("buffer granted");
            capability
        })
        .collect()
}

fn region_carrying(payload: &[u8]) -> MemoryRegion {
    let resource = Resource::self_resource().expect("self resource");
    let region = resource
        .create_virtual(USERLAND_PAGE_SIZE, runtime::MemoryAccess::RW)
        .expect("create imported region");
    let mapped = region
        .map_full(UserMemFlags::ReadWrite)
        .expect("map imported region");
    mapped.copy_in(payload).expect("payload into the region");
    drop(mapped);
    region
}

#[kernel_test]
fn filesystem_serves_an_imported_region_left_without_a_mapping() {
    const PAYLOAD: &[u8] = b"degraded";
    const IMPORTED: &str = "/imported";
    const FILLER_LIMIT: usize = 96;
    const RELEASED_SESSIONS: usize = 3;
    const REAP_ATTEMPTS: usize = 64;
    const REAP_TURN: Timeout = Timeout::from_millis(5);

    // Сценарий не возвращает занятые слоты. Закреплённые регионы держат их до конца жизни процесса,
    // поэтому дерево берётся отдельным экземпляром, а не общей точкой монтирования.
    mount_filesystem(SPARE_MOUNT, SPARE_FILESYSTEM).expect("fixture mounted");
    let capability = session_for(SPARE_MOUNT);
    let client = connect(&capability);
    // Буфер сессии создаётся до наполнения, поэтому его маппинг занимает слот наравне с остальными.
    let data_region = acquire_buffer(&client);
    let data = data_region
        .map_full(UserMemFlags::ReadWrite)
        .expect("map data buffer");

    // Дерево создаётся заранее, потому что под исчерпанными слотами куче сервера уже не вырасти. У
    // адресного пространства конечное число слотов, и наполнение упирается в них задолго до предела
    // имён.
    let names: Vec<String> = (0..FILLER_LIMIT)
        .map(|index| format!("/filler-{index}"))
        .collect();
    create_filler_tree(&client, &data, &names);
    // Закрытие сессии освободит слот, и капы берутся заранее, потому что под исчерпанными слотами
    // буфер уже не создать. Их несколько. За освободившийся слот состязаются и другие маппинги
    // сервера.
    let spare_sessions = buffered_sessions(SPARE_MOUNT, RELEASED_SESSIONS);

    // Сервер держит регион под содержимым файла отображённым, и слоты пропадают по одному на файл.
    let fillers = occupy_mapping_slots(&client, &names);
    kernel_tests::kassert!(fillers.len() < FILLER_LIMIT);

    // Отобразить принесённый регион серверу нечем, и файл всё равно создаётся.
    client
        .create_backed(
            path(IMPORTED),
            region_carrying(PAYLOAD),
            PAYLOAD.len() as u64,
        )
        .expect("create_backed call")
        .value
        .expect("registration outlives the refused mapping");

    let imported = open(&client, IMPORTED, AccessMode::ReadWrite, OpenFlags::NONE);
    let length = PAYLOAD.len() as u32;
    // Без резидентного маппинга обращение идёт за слотом и упирается в то же исчерпание.
    kernel_tests::kassert_eq!(
        client
            .read(imported.fd, 0, length)
            .expect("read call")
            .value,
        Err(FileSystemError::NoSpace)
    );
    // Файл на резидентном регионе читается, потому что сервер обслуживает сессию, и отказ выше
    // принадлежит отложенному маппингу, а не циклу.
    let resident = open(
        &client,
        fillers.last().expect("filling consumed at least one file"),
        AccessMode::Read,
        OpenFlags::NONE,
    );
    let count = client
        .read(resident.fd, 0, 1)
        .expect("read call")
        .value
        .expect("resident store reads");
    kernel_tests::kassert_eq!(count, 1);
    close(&client, &resident);

    // Закрытие сессий возвращает слоты их буферов, а уборка идёт своим чередом, поэтому слот
    // появляется не в тот же кадр.
    drop(spare_sessions);
    let turn = Signal::create().expect("signal create");
    let mut read = client
        .read(imported.fd, 0, length)
        .expect("read call")
        .value;
    for _ in 0..REAP_ATTEMPTS {
        if read.is_ok() {
            break;
        }
        let _ = turn.wait(syscall::SIGNALED, REAP_TURN);
        read = client
            .read(imported.fd, 0, length)
            .expect("read call")
            .value;
    }

    // Слот появился, и отложенный маппинг доносит содержимое региона, который принесли.
    let count = read.expect("degraded store maps on demand");
    kernel_tests::kassert_eq!(count, length);
    kernel_tests::kassert_eq!(buffer_head(&data, PAYLOAD.len()), PAYLOAD);
}

#[kernel_test]
fn filesystem_refuses_to_change_the_image() {
    let capability = image_session();
    let client = connect(&capability);

    // Изменить образ нельзя, и дескриптор на запись не выдаётся, поэтому отказ приходит уже на
    // открытии.
    let writable = client
        .open(path(IMAGE_FILE), AccessMode::ReadWrite, OpenFlags::NONE)
        .expect("open call")
        .value;
    kernel_tests::kassert_eq!(writable, Err(FileSystemError::ReadOnly));

    // Усечение без режима записи - ошибка режима, потому что она первичнее неизменяемости дерева.
    let truncated = client
        .open(path(IMAGE_FILE), AccessMode::Read, OpenFlags::TRUNCATE)
        .expect("open call")
        .value;
    kernel_tests::kassert_eq!(truncated, Err(FileSystemError::WrongMode));

    let created = client
        .open(path("/boot/fresh"), AccessMode::Write, OpenFlags::CREATE)
        .expect("open call")
        .value;
    kernel_tests::kassert_eq!(created, Err(FileSystemError::ReadOnly));

    let removed = client
        .unlink(path(IMAGE_FILE), NodeKind::File)
        .expect("unlink call")
        .value;
    kernel_tests::kassert_eq!(removed, Err(FileSystemError::ReadOnly));

    // Причина пути важнее причины режима, потому что изменяемое дерево даёт те же отказы.
    let absent = client
        .unlink(path("/boot/absent"), NodeKind::File)
        .expect("unlink call")
        .value;
    kernel_tests::kassert_eq!(absent, Err(FileSystemError::NotFound));

    let through_a_file = client
        .unlink(path("/boot/testrunner/sample/inner"), NodeKind::File)
        .expect("unlink call")
        .value;
    kernel_tests::kassert_eq!(through_a_file, Err(FileSystemError::NotADirectory));
}

#[kernel_test]
fn filesystem_holds_a_descriptor_to_its_mode() {
    let capability = ram_session();
    let client = connect(&capability);
    let region = acquire_buffer(&client);
    let buffer = region
        .map_full(UserMemFlags::ReadWrite)
        .expect("map data buffer");

    let opened = open(&client, "/write-only", AccessMode::Write, OpenFlags::CREATE);
    // Режим записи работает. Отказы ниже - про отсутствующий режим чтения, а не про сам дескриптор.
    let payload = b"mode";
    let count = write_all(&client, &buffer, &opened, 0, payload);
    kernel_tests::kassert_eq!(count, payload.len() as u32);

    let read = client.read(opened.fd, 0, 4).expect("read call").value;
    kernel_tests::kassert_eq!(read, Err(FileSystemError::WrongMode));

    let stated = client.stat(opened.fd).expect("stat call").value;
    kernel_tests::kassert_eq!(stated, Err(FileSystemError::WrongMode));

    // Режим важнее наличия окна. У узла ramfs нет его, но режим проверяется.
    let window = client
        .backing_memory(opened.fd)
        .expect("backing_memory call")
        .value;
    kernel_tests::kassert_eq!(window, Err(FileSystemError::WrongMode));

    let directory = open(
        &client,
        "/write-only-dir",
        AccessMode::Write,
        OpenFlags::CREATE | OpenFlags::DIRECTORY,
    );
    let listed = client
        .list(directory.fd, DirCursor::START)
        .expect("list call")
        .value;
    kernel_tests::kassert_eq!(listed, Err(FileSystemError::WrongMode));
}

#[kernel_test]
fn filesystem_appends_to_the_end_of_a_file() {
    let capability = ram_session();
    let client = connect(&capability);
    let region = acquire_buffer(&client);
    let buffer = region
        .map_full(UserMemFlags::ReadWrite)
        .expect("map data buffer");

    let opened = open(
        &client,
        "/appended",
        AccessMode::ReadWrite,
        OpenFlags::CREATE,
    );
    let head = b"head";
    write_all(&client, &buffer, &opened, 0, head);

    let tail = b"-tail";
    buffer.copy_in(tail).expect("tail into the buffer");
    let written = client
        .write(opened.fd, APPEND_OFFSET, tail.len() as u32)
        .expect("write call")
        .value
        .expect("write ok");
    kernel_tests::kassert_eq!(written.offset, head.len() as u64);
    kernel_tests::kassert_eq!(written.count, tail.len() as u32);

    let info = client
        .stat(opened.fd)
        .expect("stat call")
        .value
        .expect("stat ok");
    kernel_tests::kassert_eq!(info.size, (head.len() + tail.len()) as u64);
}

#[kernel_test]
fn filesystem_rejects_a_stale_descriptor_after_slot_reuse() {
    let capability = ram_session();
    let client = connect(&capability);

    let stale = open(
        &client,
        "/stale-first",
        AccessMode::ReadWrite,
        OpenFlags::CREATE,
    );
    close(&client, &stale);
    let fresh = open(
        &client,
        "/stale-second",
        AccessMode::ReadWrite,
        OpenFlags::CREATE,
    );

    // Слот занят другим узлом, а отставший вызов закрытым дескриптором до него не достаёт.
    let refused = client.stat(stale.fd).expect("stat call").value;
    kernel_tests::kassert_eq!(refused, Err(FileSystemError::BadDescriptor));
    let alive = client
        .stat(fresh.fd)
        .expect("stat call")
        .value
        .expect("stat ok");
    kernel_tests::kassert_eq!(alive.kind, NodeKind::File);
}

#[kernel_test]
fn filesystem_isolates_descriptors_between_sessions() {
    let owner_capability = ram_session();
    let owner = connect(&owner_capability);
    let stranger_capability = ram_session();
    let stranger = connect(&stranger_capability);

    let opened = open(
        &owner,
        "/isolated",
        AccessMode::ReadWrite,
        OpenFlags::CREATE,
    );
    let mine = owner
        .stat(opened.fd)
        .expect("stat call")
        .value
        .expect("stat ok");
    kernel_tests::kassert_eq!(mine.kind, NodeKind::File);

    let foreign = stranger.stat(opened.fd).expect("stat call").value;
    kernel_tests::kassert_eq!(foreign, Err(FileSystemError::BadDescriptor));
}

#[kernel_test]
fn filesystem_serves_on_after_an_undeliverable_reply() {
    let abandoned_capability = ram_session();
    let abandoned = connect_abandoning(&abandoned_capability);
    // Этот клиент не получит ответа, но сервер обязан создать узел, иначе кадр не дошёл до
    // обработчика и недоставленный ответ не проверен.
    let _ = abandoned.open(
        path(ABANDONED_PATH),
        AccessMode::ReadWrite,
        OpenFlags::CREATE,
    );

    let capability = ram_session();
    let client = connect(&capability);
    let opened = open(&client, ABANDONED_PATH, AccessMode::Read, OpenFlags::NONE);
    kernel_tests::kassert_eq!(opened.kind, NodeKind::File);
}

#[kernel_test]
fn filesystem_serves_on_after_an_unknown_ordinal() {
    let bogus_capability = ram_session();
    let bogus = connect_tampered(
        &bogus_capability,
        Tamper {
            unknown_ordinal: true,
            ..Tamper::NONE
        },
    );
    // Контракт не описывает такой операции, поэтому сервер закрывает соединение кадром peer-close,
    // без захода в вызов собственного порта. Аргументы вызова не играют роли - до операции кадр не
    // доходит.
    let refused = bogus
        .stat(NodeDescriptor::from_raw(0))
        .map(|response| response.value);
    kernel_tests::kassert_eq!(refused, Err(IpcError::PeerClosed));

    let capability = ram_session();
    let client = connect(&capability);
    let opened = open(
        &client,
        "/after-unknown-ordinal",
        AccessMode::ReadWrite,
        OpenFlags::CREATE,
    );
    kernel_tests::kassert_eq!(opened.kind, NodeKind::File);
}

#[kernel_test]
fn filesystem_releases_the_reply_of_an_unparsed_frame() {
    let capability = ram_session();
    let malformed = connect_tampered(
        &capability,
        Tamper {
            empty_body: true,
            ..Tamper::NONE
        },
    );
    // Кадр без полей до обработчика не доходит. Закрытие reply-объекта будит
    // вызывателя отказом до его собственного тайм-аута.
    let refused = malformed
        .stat(NodeDescriptor::from_raw(0))
        .map(|response| response.value);
    kernel_tests::kassert_eq!(refused, Err(IpcError::PeerClosed));
}

fn send_unparsed_frame_with_capability<K: kernel_objects::ObjectKind>(
    session: &Port,
    capability: OwnedHandle<K>,
) {
    let mut frame = [0u8; MESSAGE_INLINE_MAX];
    Header::new(u64::MAX, 0, 0)
        .encode(&mut frame)
        .expect("frame header");
    frame[HEADER_SIZE] = TERMINATOR;
    // Отправка идёт сырым syscall, потому что транспорт сводит любой отказ сервера к PeerClosed.
    crate::ipc_raw::write_frame(&frame[..=HEADER_SIZE], &[capability.into_raw().raw()]);
    kernel_tests::kassert_eq!(port_send(session.as_raw(), CALL_TIMEOUT), 0);
}

#[kernel_test]
fn filesystem_closes_the_capability_of_an_unparsed_frame() {
    let capability = ram_session();
    let signal = Signal::create().expect("signal to ride the frames");
    for _ in 0..LEAK_FRAMES {
        // Капа оказалась в кадре, который не разобран, поэтому закрыть её больше некому.
        let rider = signal
            .duplicate(Rights::READ | Rights::TRANSFER, None)
            .expect("capability for the frame");
        send_unparsed_frame_with_capability(&capability, rider);
    }
    kernel_tests::kassert!(vends_a_buffer());
}

#[kernel_test]
fn filesystem_closes_the_capability_of_an_undelivered_reply() {
    let capability = image_session();
    let client = connect(&capability);
    let opened = open(&client, IMAGE_FILE, AccessMode::Read, OpenFlags::NONE);

    // Обе стороны говорят по одной капе, и дескриптор у них общий. Разница только в том, что второй
    // не ждёт ответа.
    let abandoned = connect_abandoning(&capability);
    for _ in 0..LEAK_FRAMES {
        // Окно передаётся в ответе, доставить который некому, поэтому выданный хэндл остаётся у
        // сервера.
        let _ = abandoned.backing_memory(opened.fd);
    }
    kernel_tests::kassert!(vends_a_buffer());
}

#[kernel_test]
fn filesystem_does_not_pin_an_undelivered_backing() {
    let capability = ram_session();
    let client = connect(&capability);
    let data_region = acquire_buffer(&client);
    let data = data_region
        .map_full(UserMemFlags::ReadWrite)
        .expect("map data buffer");
    let opened = open(
        &client,
        "/undelivered-backing",
        AccessMode::ReadWrite,
        OpenFlags::CREATE,
    );
    write_all(&client, &data, &opened, 0, b"body");

    let abandoned = connect_abandoning(&capability);
    let _ = abandoned.backing_memory(opened.fd);

    data.copy_in(b"x").expect("payload into data buffer");
    let grown = client
        .write(opened.fd, USERLAND_PAGE_SIZE, 1)
        .expect("write call")
        .value
        .expect("undelivered backing does not pin");
    kernel_tests::kassert_eq!(grown.count, 1);

    let mut response = client
        .backing_memory(opened.fd)
        .expect("backing_memory call");
    let capability = response.value.expect("backing granted");
    let backing = MemoryRegion::from(response.claim(capability).expect("capability owner"));
    let capacity = backing.inspect().expect("inspect backing").size_bytes;
    data.copy_in(b"y").expect("payload into data buffer");
    let pinned = client
        .write(opened.fd, capacity, 1)
        .expect("write call")
        .value;
    kernel_tests::kassert_eq!(pinned, Err(FileSystemError::NoSpace));
}

fn vends_a_buffer() -> bool {
    let capability = image_session();
    let client = connect(&capability);
    // Выданный буфер закрывает ответ: проверке довольно факта выдачи.
    let Ok(granted) = client.acquire_buffer(BUFFER_SIZE) else {
        return false;
    };
    granted.value.is_ok()
}

fn take_and_drop_session() {
    let capability = ram_session();
    let client = connect(&capability);
    let mut response = client
        .acquire_buffer(REAP_BUFFER_SIZE)
        .expect("acquire_buffer call");
    let capability = response.value.expect("buffer granted");
    let buffer = response.claim(capability).expect("capability owner");
    drop(MemoryRegion::from(buffer));
}

fn run_client_fixture() {
    let client = spawn_from_image(CLIENT_FIXTURE, ram_session());
    client
        .join(CLIENT_TIMEOUT)
        .expect("client fixture terminates");
    kernel_tests::kassert_eq!(client.exit_code().expect("exit code"), 0);
}

#[kernel_test]
fn filesystem_reaps_the_session_of_a_terminated_client() {
    // Завершение процесса освобождает его капы, поэтому сессия закрывается вместе с буфером и
    // дескрипторами, без капы на процесс и без отдельного потока. Цикл сходится только на сервере,
    // который убирает закрывшиеся сессии, потому что их буферы держат бюджет монтирования.
    for _ in 0..REAP_SESSIONS {
        run_client_fixture();
    }
}

#[kernel_test]
fn filesystem_reaps_the_session_of_a_closed_capability() {
    // Живой клиент закрыл единственную капу значка. Для сервера это то же закрытие стороны, что и
    // завершение процесса.
    for _ in 0..REAP_SESSIONS {
        take_and_drop_session();
    }
}

#[kernel_test]
fn filesystem_reaps_only_the_session_of_the_gone_badge() {
    let keeper_capability = ram_session();
    let keeper = connect(&keeper_capability);
    let kept = open(&keeper, "/kept", AccessMode::ReadWrite, OpenFlags::CREATE);
    let keeper_buffer = acquire_buffer(&keeper);

    for _ in 0..REAP_SESSIONS {
        take_and_drop_session();
    }

    // Значок наблюдателя не пропадал, поэтому его сессия, дескрипторы и буфер на месте.
    let mine = keeper
        .stat(kept.fd)
        .expect("stat call")
        .value
        .expect("stat ok");
    kernel_tests::kassert_eq!(mine.kind, NodeKind::File);
    let occupied = keeper
        .acquire_buffer(BUFFER_SIZE)
        .expect("acquire_buffer call")
        .value;
    kernel_tests::kassert_eq!(occupied, Err(FileSystemError::AlreadyExists));
    drop(keeper_buffer);
}
