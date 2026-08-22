//! E2E файловой подсистемы: rootkeeper поднимает файловые системы, а маршрутизатор получает их
//! сессии у supervisor и владеет таблицей монтирования.

use alloc::{boxed::Box, format, string::String, vec, vec::Vec};

use bootstrap_abi::bootstrap;
use filesystem_abi::{
    AccessMode, DirCursor, DirEntry, FileSystemError, NAME_MAX, NodeDescriptor, NodeKind,
    OpenFlags, Opened, Path, file_system, namespace, namespace_admin,
};
use ipc::{
    MessageLen, Transport,
    wire::{HEADER_SIZE, Header, IpcError, MESSAGE_INLINE_MAX, Str, TERMINATOR},
};
use kernel_tests::kernel_test;
use managed_component::ComponentRoots;
use rootkeeper::{RootSystem, SystemImage};
use runtime::{
    BorrowedHandle, MemoryRegion, OwnedHandle, PortTransport, Process, Resource, Signal, Timeout,
    UserMemFlags, port_send,
};
use spin::Once;
use syscall::Rights;
use userland::USERLAND_PAGE_SIZE;
use userland_image::{component_name, decode};
use userland_loader::load_entry;

pub(crate) const CALL_TIMEOUT: Timeout = Timeout::from_secs(2);

const CLIENT_TIMEOUT: Timeout = Timeout::from_secs(2);

const CLIENT_FIXTURE: &str = "/boot/filesystem-client-fixture/program";

const RAM_MOUNT: &str = "/tmp";
const RAM_FILESYSTEM_PROGRAM: &str = "/boot/filesystem-ramfs";
const UNRESPONSIVE_FILESYSTEM_PROGRAM: &str = "/boot/unresponsive-filesystem";
const SPARE_MOUNT: &str = "/spare";
const SPARE_FILESYSTEM_PROGRAM: &str = "/boot/spare-filesystem";

const APPEND_OFFSET: u64 = u64::MAX;

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

fn router_minter() -> BorrowedHandle<'static> {
    namespace_roots().service_minter()
}

fn router_admin() -> BorrowedHandle<'static> {
    namespace_roots().control()
}

fn namespace_ports() -> &'static NamespacePorts {
    NAMESPACE_PORTS.call_once(start_namespace)
}

fn namespace_roots() -> &'static ComponentRoots {
    namespace_ports()
        .system
        .component(0)
        .expect("composition carries the namespace router")
        .managed()
        .running()
        .expect("namespace router is running")
        .roots()
}

fn namespace_admin_client(
    capability: BorrowedHandle<'_>,
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
    let system = RootSystem::bring_up(
        image,
        plan,
        crate::bootstrap_handle(),
        crate::log_relay::component_log_session,
    )
    .expect("filesystem composition started");
    NamespacePorts { system }
}

fn spawn_from_image(path: &str, start: OwnedHandle) -> Process {
    let bootstrap = bootstrap::Client::new(PortTransport::client(crate::bootstrap_handle()));
    let region = MemoryRegion::adopt(acquire_image(&bootstrap));
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

fn acquire_image(bootstrap: &bootstrap::Client<PortTransport>) -> ipc::wire::Cap {
    bootstrap
        .acquire_userland_image()
        .expect("acquire_userland_image call")
        .expect("image vend")
}

fn namespace_client(capability: BorrowedHandle<'_>) -> namespace::Client<PortTransport> {
    namespace::Client::new(PortTransport::client(capability)).with_wait(CALL_TIMEOUT)
}

pub(crate) fn mount_ram_filesystem(prefix: &str) -> Result<u32, FileSystemError> {
    namespace_admin_client(router_admin())
        .mount(path(prefix), path(RAM_FILESYSTEM_PROGRAM))
        .expect("mount call")
}

pub(crate) fn unmount_filesystem(prefix: &str) -> Result<u32, FileSystemError> {
    namespace_admin_client(router_admin())
        .unmount(path(prefix))
        .expect("unmount call")
}

pub(crate) fn session_for(text: &str) -> OwnedHandle {
    let session = namespace_client(router_minter())
        .resolve(path(text))
        .expect("resolve call")
        .expect("path is covered by a mount");
    OwnedHandle::adopt(session)
}

pub(crate) fn ram_session() -> OwnedHandle {
    session_for(RAM_MOUNT)
}

pub(crate) fn image_session() -> OwnedHandle {
    session_for(Path::ROOT.as_str())
}

pub(crate) fn connect(capability: &OwnedHandle) -> file_system::Client<PortTransport> {
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

fn connect_tampered(capability: &OwnedHandle, tamper: Tamper) -> file_system::Client<Tampered> {
    file_system::Client::new(Tampered {
        port: PortTransport::client(capability),
        tamper,
    })
    .with_wait(CALL_TIMEOUT)
}

fn connect_abandoning(capability: &OwnedHandle) -> file_system::Client<Tampered> {
    connect_tampered(
        capability,
        Tamper {
            one_way: true,
            ..Tamper::NONE
        },
    )
}

pub(crate) fn acquire_buffer(client: &file_system::Client<PortTransport>) -> MemoryRegion {
    let cap = client
        .acquire_buffer(BUFFER_SIZE)
        .expect("acquire_buffer call")
        .expect("buffer granted");
    MemoryRegion::adopt(cap)
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
        .expect("open ok")
}

fn close(client: &file_system::Client<PortTransport>, opened: &Opened) {
    client
        .close(opened.fd)
        .expect("close call")
        .expect("close ok");
}

pub(crate) fn write_all(
    client: &file_system::Client<PortTransport>,
    buffer: &runtime::Mapping,
    opened: &Opened,
    offset: u64,
    payload: &[u8],
) -> u32 {
    buffer.copy_in(payload).expect("payload into the buffer");
    let written = client
        .write(opened.fd, offset, payload.len() as u32)
        .expect("write call")
        .expect("write ok");
    written.count
}

pub(crate) fn buffer_head(buffer: &runtime::Mapping, len: usize) -> Vec<u8> {
    let mut bytes = vec![0; len];
    buffer.copy_out(&mut bytes).expect("buffer head");
    bytes
}

fn buffer_bytes(buffer: &runtime::Mapping) -> Vec<u8> {
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
    buffer: &runtime::Mapping,
    fd: NodeDescriptor,
) -> Vec<String> {
    let mut names = Vec::new();
    let mut from = DirCursor::START;
    loop {
        let listed = client.list(fd, from).expect("list call").expect("list ok");
        collect_entries(&buffer_bytes(buffer), listed.count, &mut names);
        if listed.end {
            return names;
        }
        // Перечисление обязано продвинуться: иначе записи не помещаются в буфер и повтор вызова
        // ничего не изменит.
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

    let info = client.stat(opened.fd).expect("stat call").expect("stat ok");
    kernel_tests::kassert_eq!(info.size, payload.len() as u64);

    let again = client
        .open(
            path("/created"),
            AccessMode::Read,
            OpenFlags::CREATE | OpenFlags::EXCLUSIVE,
        )
        .expect("open call");
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
        .expect("read ok");
    kernel_tests::kassert_eq!(count, 6);
    kernel_tests::kassert_eq!(buffer_head(&buffer, 6), b"456789");

    let tail = client
        .read(opened.fd, payload.len() as u64, 4)
        .expect("read call")
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

    let file_info = client.stat(file.fd).expect("stat call").expect("stat ok");
    kernel_tests::kassert_eq!(file_info.kind, NodeKind::File);
    kernel_tests::kassert_eq!(file_info.size, 4);

    let directory_info = client
        .stat(directory.fd)
        .expect("stat call")
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
            .expect("list ok");
        batches += 1;
        collect_entries(&buffer_bytes(&buffer), listed.count, &mut names);
        if listed.end {
            break;
        }
        // Перечисление обязано продвинуться: иначе записи не помещаются в
        // буфер и повтор вызова ничего не изменит.
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
        .unlink(path("/removed"))
        .expect("unlink call")
        .expect("unlink ok");

    let reopened = client
        .open(path("/removed"), AccessMode::Read, OpenFlags::NONE)
        .expect("open call");
    kernel_tests::kassert_eq!(reopened, Err(FileSystemError::NotFound));
    let again = client.unlink(path("/removed")).expect("unlink call");
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
        .expect("write call");
    kernel_tests::kassert_eq!(written, Err(FileSystemError::WrongMode));

    let resized = client.set_size(opened.fd, 16).expect("set_size call");
    kernel_tests::kassert_eq!(resized, Err(FileSystemError::WrongMode));

    // Усечение меняет узел: без режима записи оно отвергается ещё на открытии.
    let truncated = client
        .open(path("/read-only"), AccessMode::Read, OpenFlags::TRUNCATE)
        .expect("open call");
    kernel_tests::kassert_eq!(truncated, Err(FileSystemError::WrongMode));
}

#[kernel_test]
fn namespace_resolves_each_path_to_the_longest_prefix() {
    let namespace = namespace_client(router_minter());

    // Каталог образа существует только в корневой imagefs.
    let image = namespace
        .resolve(path(IMAGE_DIRECTORY))
        .expect("resolve call")
        .expect("root mount covers the path");
    drop(OwnedHandle::adopt(image));

    let ram = ram_session();
    connect(&ram)
        .open(
            path("/inner"),
            AccessMode::ReadWrite,
            OpenFlags::CREATE | OpenFlags::DIRECTORY,
        )
        .expect("create directory call")
        .expect("directory created");

    // `/tmp/inner` отсутствует в imagefs: успешное окно доказывает выбор длинного префикса.
    let ram = namespace
        .resolve(path("/tmp/inner"))
        .expect("resolve call")
        .expect("ram mount covers the path");
    drop(OwnedHandle::adopt(ram));
}

#[kernel_test]
fn namespace_refuses_an_occupied_prefix_without_changing_the_table() {
    let namespace = namespace_admin_client(router_admin());

    let occupied = namespace
        .mount(path(RAM_MOUNT), path(RAM_FILESYSTEM_PROGRAM))
        .expect("mount call");
    kernel_tests::kassert_eq!(occupied, Err(FileSystemError::AlreadyExists));

    let mounted = namespace_client(router_minter())
        .resolve(path(RAM_MOUNT))
        .expect("resolve call")
        .expect("original mount remains");
    drop(OwnedHandle::adopt(mounted));
}

#[kernel_test]
fn namespace_rejects_admin_operations_on_the_service_port() {
    // Управление таблицей монтирования живёт на control-порту: его ordinal service-порт не
    // разбирает, и кадр отвергается целиком, не доходя до операции.
    let admin_on_service = namespace_admin_client(router_minter());
    let refused = admin_on_service.unmount(path("/"));
    kernel_tests::kassert_eq!(refused, Err(IpcError::PeerClosed));

    // Отвергнутый кадр не тронул ни таблицу, ни цикл обслуживания: корень резолвится дальше.
    let mounted = namespace_client(router_minter())
        .resolve(path(IMAGE_FILE))
        .expect("resolve call")
        .expect("root mount survives the rejected frame");
    drop(OwnedHandle::adopt(mounted));
}

#[kernel_test]
fn namespace_reports_an_unresponsive_mount() {
    const STUCK_MOUNT: &str = "/boot";

    let namespace = namespace_admin_client(router_admin());
    namespace
        .mount(path(STUCK_MOUNT), path(UNRESPONSIVE_FILESYSTEM_PROGRAM))
        .expect("mount call")
        .expect("fixture mounted");

    // Сессия остаётся живой без обслуживающего потока: `open_root` не отвечает, и маршрутизатор
    // отпускает вызов по своему сроку.
    let resolved = namespace_client(router_minter())
        .resolve(path(STUCK_MOUNT))
        .expect("resolve call");
    kernel_tests::kassert_eq!(resolved, Err(FileSystemError::Unresponsive));

    // Снятие отпускает зависшую сессию, и путь снова накрывает корневое монтирование.
    namespace
        .unmount(path(STUCK_MOUNT))
        .expect("unmount call")
        .expect("fixture unmounted");
    let rerouted = namespace_client(router_minter())
        .resolve(path(STUCK_MOUNT))
        .expect("resolve call")
        .expect("root mount covers the path again");
    drop(OwnedHandle::adopt(rerouted));
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
        .expect("existing session keeps its root");
    kernel_tests::kassert_eq!(opened.kind, NodeKind::Directory);
    let rerouted = namespace_client(router_minter())
        .resolve(path(DETACHED_MOUNT))
        .expect("resolve call")
        .expect("root mount covers the path after unmount");
    drop(OwnedHandle::adopt(rerouted));
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

    // Каждая программа образа лежит своим каталогом: под `/boot` перечисляются
    // они, а её записи - внутри.
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

    // Монтирования держат разные процессы: узел одного в дереве другого не виден, а корень
    // каждого - корень его собственного монтирования.
    let absent = image
        .open(path("/only-in-ram"), AccessMode::Read, OpenFlags::NONE)
        .expect("open call");
    kernel_tests::kassert_eq!(absent, Err(FileSystemError::NotFound));
    let foreign = ram
        .open(path(IMAGE_FILE), AccessMode::Read, OpenFlags::NONE)
        .expect("open call");
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
        .expect("read ok");

    let window = MemoryRegion::adopt(
        client
            .backing_memory(opened.fd)
            .expect("backing_memory call")
            .expect("window granted"),
    );
    let mapped = window
        .map_full(UserMemFlags::ReadOnly)
        .expect("map backing window");
    // Маппинг округлён вверх до страницы, настоящую длину даёт `Opened.size`.
    kernel_tests::kassert!(mapped.size_bytes() >= opened.size);
    kernel_tests::kassert_eq!(
        buffer_head(&mapped, opened.size as usize),
        buffer_head(&buffer, count as usize)
    );

    // Дескриптор открыт на чтение, регион образа - read-only: писать в страницы
    // напрямую капа не даёт.
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

    let info = client.stat(opened.fd).expect("stat call").expect("stat ok");
    kernel_tests::kassert_eq!(info.kind, NodeKind::File);
    kernel_tests::kassert_eq!(info.size, 0);

    let count = client
        .read(opened.fd, 0, BUFFER_SIZE)
        .expect("read call")
        .expect("read ok");
    kernel_tests::kassert_eq!(count, 0);

    // Файл без байт не занимает страниц образа: окна у него нет.
    let refused = client
        .backing_memory(opened.fd)
        .expect("backing_memory call");
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
        .expect("backing_memory call");
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

    let backing = MemoryRegion::adopt(
        client
            .backing_memory(opened.fd)
            .expect("backing_memory call")
            .expect("backing granted"),
    );
    let mapped = backing
        .map_full(UserMemFlags::ReadWrite)
        .expect("map ramfs backing");
    mapped.copy_in(b"mapped").expect("write through mapping");
    let count = client
        .read(opened.fd, 0, 6)
        .expect("read call")
        .expect("read ok");
    kernel_tests::kassert_eq!(count, 6);
    kernel_tests::kassert_eq!(buffer_head(&data, 6), b"mapped");

    write_all(&client, &data, &opened, 0, b"server");
    kernel_tests::kassert_eq!(buffer_head(&mapped, 6), b"server");

    data.copy_in(b"x").expect("payload into data buffer");
    let beyond = client
        .write(opened.fd, mapped.size_bytes(), 1)
        .expect("write call");
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

    let backing = MemoryRegion::adopt(
        client
            .backing_memory(opened.fd)
            .expect("backing_memory call")
            .expect("backing granted"),
    );
    let mapped = backing
        .map_full(UserMemFlags::ReadWrite)
        .expect("map ramfs backing");
    // Разрыв заполнен через маппинг: запись за прежней длиной обязана вернуть туда нули.
    mapped.copy_in_at(4, b"stale").expect("stale bytes");

    data.copy_in(b"tail").expect("payload into data buffer");
    let written = client
        .write(opened.fd, 16, 4)
        .expect("write call")
        .expect("sparse write within capacity");
    kernel_tests::kassert_eq!(written.count, 4);

    let count = client
        .read(opened.fd, 0, 20)
        .expect("read call")
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
        .create_backed(path("/client-region"), region.into_handle(), 6)
        .expect("create_backed call")
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
        .expect("read ok");
    kernel_tests::kassert_eq!(count, 6);
    kernel_tests::kassert_eq!(buffer_head(&data, 6), b"client");

    mapped.copy_in(b"mapped").expect("change client mapping");
    let count = reader
        .read(opened.fd, 0, 6)
        .expect("read call")
        .expect("read ok");
    kernel_tests::kassert_eq!(count, 6);
    kernel_tests::kassert_eq!(buffer_head(&data, 6), b"mapped");

    write_all(&reader, &data, &opened, 0, b"server");
    kernel_tests::kassert_eq!(buffer_head(&mapped, 6), b"server");

    let shared = MemoryRegion::adopt(
        reader
            .backing_memory(opened.fd)
            .expect("backing_memory call")
            .expect("backing granted"),
    );
    kernel_tests::kassert!(
        shared
            .handle()
            .duplicate(Rights::READ | Rights::TRANSFER, None)
            .is_err()
    );
}

fn create_filler_tree(
    client: &file_system::Client<PortTransport>,
    data: &runtime::Mapping,
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
        match backed {
            Ok(cap) => {
                drop(OwnedHandle::adopt(cap));
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

fn buffered_sessions(mount: &str, count: usize) -> Vec<OwnedHandle> {
    (0..count)
        .map(|_| {
            let capability = session_for(mount);
            let buffer = connect(&capability)
                .acquire_buffer(BUFFER_SIZE)
                .expect("acquire_buffer call")
                .expect("buffer granted");
            drop(OwnedHandle::adopt(buffer));
            capability
        })
        .collect()
}

fn region_carrying(payload: &[u8]) -> OwnedHandle {
    let resource = Resource::self_resource().expect("self resource");
    let region = resource
        .create_virtual(USERLAND_PAGE_SIZE, runtime::MemoryAccess::RW)
        .expect("create imported region");
    let mapped = region
        .map_full(UserMemFlags::ReadWrite)
        .expect("map imported region");
    mapped.copy_in(payload).expect("payload into the region");
    drop(mapped);
    region.into_handle()
}

#[kernel_test]
fn filesystem_serves_an_imported_region_left_without_a_mapping() {
    const PAYLOAD: &[u8] = b"degraded";
    const IMPORTED: &str = "/imported";
    const FILLER_LIMIT: usize = 96;
    const RELEASED_SESSIONS: usize = 3;
    const REAP_ATTEMPTS: usize = 64;
    const REAP_TURN: Timeout = Timeout::from_millis(5);

    // Занятые слоты сценарий не возвращает: закреплённые регионы держат их до конца жизни
    // процесса, поэтому дерево берётся отдельным экземпляром, а не общей точкой монтирования.
    namespace_admin_client(router_admin())
        .mount(path(SPARE_MOUNT), path(SPARE_FILESYSTEM_PROGRAM))
        .expect("mount call")
        .expect("fixture mounted");
    let capability = session_for(SPARE_MOUNT);
    let client = connect(&capability);
    // Буфер сессии заводится до наполнения: его маппинг занимает слот наравне с остальными.
    let data_region = acquire_buffer(&client);
    let data = data_region
        .map_full(UserMemFlags::ReadWrite)
        .expect("map data buffer");

    // Дерево заводится заранее: под исчерпанными слотами куче сервера уже не вырасти. Слотов у
    // адресного пространства конечное число, и наполнение упирается в них задолго до предела имён.
    let names: Vec<String> = (0..FILLER_LIMIT)
        .map(|index| format!("/filler-{index}"))
        .collect();
    create_filler_tree(&client, &data, &names);
    // Освободит слот уход сессии, и капы берутся заранее: под исчерпанными слотами буфер уже не
    // завести. Их несколько: за освободившийся слот состязаются и другие маппинги сервера.
    let spare_sessions = buffered_sessions(SPARE_MOUNT, RELEASED_SESSIONS);

    // Регион под содержимым файла сервер держит отображённым, и слоты уходят по одному на файл.
    let fillers = occupy_mapping_slots(&client, &names);
    kernel_tests::kassert!(fillers.len() < FILLER_LIMIT);

    // Отобразить принесённый регион серверу нечем, и файл всё равно заводится.
    client
        .create_backed(
            path(IMPORTED),
            region_carrying(PAYLOAD),
            PAYLOAD.len() as u64,
        )
        .expect("create_backed call")
        .expect("registration outlives the refused mapping");

    let imported = open(&client, IMPORTED, AccessMode::ReadWrite, OpenFlags::NONE);
    let length = PAYLOAD.len() as u32;
    // Без резидентного маппинга обращение идёт за слотом и упирается в то же исчерпание.
    kernel_tests::kassert_eq!(
        client.read(imported.fd, 0, length).expect("read call"),
        Err(FileSystemError::NoSpace)
    );
    // Файл на резидентном регионе читается: сервер обслуживает сессию, и отказ выше принадлежит
    // отложенному маппингу, а не циклу.
    let resident = open(
        &client,
        fillers.last().expect("filling consumed at least one file"),
        AccessMode::Read,
        OpenFlags::NONE,
    );
    let count = client
        .read(resident.fd, 0, 1)
        .expect("read call")
        .expect("resident store reads");
    kernel_tests::kassert_eq!(count, 1);
    close(&client, &resident);

    // Уход сессий возвращает слоты их буферов, а уборка идёт своим чередом: слот появляется не в
    // тот же кадр.
    drop(spare_sessions);
    let turn = Signal::create().expect("signal create");
    let mut read = client.read(imported.fd, 0, length).expect("read call");
    for _ in 0..REAP_ATTEMPTS {
        if read.is_ok() {
            break;
        }
        let _ = turn.wait(syscall::SIGNALED, REAP_TURN);
        read = client.read(imported.fd, 0, length).expect("read call");
    }

    // Слот появился, и отложенный маппинг доносит содержимое принесённого региона.
    let count = read.expect("degraded store maps on demand");
    kernel_tests::kassert_eq!(count, length);
    kernel_tests::kassert_eq!(buffer_head(&data, PAYLOAD.len()), PAYLOAD);
}

#[kernel_test]
fn filesystem_refuses_to_change_the_image() {
    let capability = image_session();
    let client = connect(&capability);

    // Изменить образ нельзя, и дескриптор на запись не выдаётся: отказ приходит
    // уже на открытии.
    let writable = client
        .open(path(IMAGE_FILE), AccessMode::ReadWrite, OpenFlags::NONE)
        .expect("open call");
    kernel_tests::kassert_eq!(writable, Err(FileSystemError::ReadOnly));

    // Усечение без режима записи - ошибка режима: она первичнее неизменяемости дерева.
    let truncated = client
        .open(path(IMAGE_FILE), AccessMode::Read, OpenFlags::TRUNCATE)
        .expect("open call");
    kernel_tests::kassert_eq!(truncated, Err(FileSystemError::WrongMode));

    let created = client
        .open(path("/boot/fresh"), AccessMode::Write, OpenFlags::CREATE)
        .expect("open call");
    kernel_tests::kassert_eq!(created, Err(FileSystemError::ReadOnly));

    let removed = client.unlink(path(IMAGE_FILE)).expect("unlink call");
    kernel_tests::kassert_eq!(removed, Err(FileSystemError::ReadOnly));

    // Причина пути важнее причины режима: те же отказы даёт изменяемое дерево.
    let absent = client.unlink(path("/boot/absent")).expect("unlink call");
    kernel_tests::kassert_eq!(absent, Err(FileSystemError::NotFound));

    let through_a_file = client
        .unlink(path("/boot/testrunner/sample/inner"))
        .expect("unlink call");
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
    // Режим записи работает: отказы ниже - про отсутствующий режим чтения, а не
    // про сам дескриптор.
    let payload = b"mode";
    let count = write_all(&client, &buffer, &opened, 0, payload);
    kernel_tests::kassert_eq!(count, payload.len() as u32);

    let read = client.read(opened.fd, 0, 4).expect("read call");
    kernel_tests::kassert_eq!(read, Err(FileSystemError::WrongMode));

    let stated = client.stat(opened.fd).expect("stat call");
    kernel_tests::kassert_eq!(stated, Err(FileSystemError::WrongMode));

    // Режим важнее наличия окна: у узла ramfs его нет, но проверяется режим.
    let window = client
        .backing_memory(opened.fd)
        .expect("backing_memory call");
    kernel_tests::kassert_eq!(window, Err(FileSystemError::WrongMode));

    let directory = open(
        &client,
        "/write-only-dir",
        AccessMode::Write,
        OpenFlags::CREATE | OpenFlags::DIRECTORY,
    );
    let listed = client
        .list(directory.fd, DirCursor::START)
        .expect("list call");
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
        .expect("write ok");
    kernel_tests::kassert_eq!(written.offset, head.len() as u64);
    kernel_tests::kassert_eq!(written.count, tail.len() as u32);

    let info = client.stat(opened.fd).expect("stat call").expect("stat ok");
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
    let refused = client.stat(stale.fd).expect("stat call");
    kernel_tests::kassert_eq!(refused, Err(FileSystemError::BadDescriptor));
    let alive = client.stat(fresh.fd).expect("stat call").expect("stat ok");
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
    let mine = owner.stat(opened.fd).expect("stat call").expect("stat ok");
    kernel_tests::kassert_eq!(mine.kind, NodeKind::File);

    let foreign = stranger.stat(opened.fd).expect("stat call");
    kernel_tests::kassert_eq!(foreign, Err(FileSystemError::BadDescriptor));
}

#[kernel_test]
fn filesystem_serves_on_after_an_undeliverable_reply() {
    let abandoned_capability = ram_session();
    let abandoned = connect_abandoning(&abandoned_capability);
    // Ответа этот клиент не получит, но узел сервер обязан завести: иначе кадр
    // не дошёл до обработчика и недоставленный ответ не проверен.
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
    // Такой операции контракт не описывает: сервер закрывает соединение кадром
    // peer-close, без захода в вызов собственного порта. Аргументы вызова
    // роли не играют - до операции кадр не доходит.
    let refused = bogus.stat(NodeDescriptor::from_raw(0));
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
    let refused = malformed.stat(NodeDescriptor::from_raw(0));
    kernel_tests::kassert_eq!(refused, Err(IpcError::PeerClosed));
}

fn send_unparsed_frame_with_capability(session: &OwnedHandle, capability: OwnedHandle) {
    let mut frame = [0u8; MESSAGE_INLINE_MAX];
    Header::new(u64::MAX, 0, 0)
        .encode(&mut frame)
        .expect("frame header");
    frame[HEADER_SIZE] = TERMINATOR;
    // Отправка идёт сырым syscall: транспорт сводит любой отказ сервера к PeerClosed.
    crate::ipc_raw::write_frame(&frame[..=HEADER_SIZE], &[capability.into_raw().raw()]);
    kernel_tests::kassert_eq!(port_send(session.as_raw(), CALL_TIMEOUT), 0);
}

#[kernel_test]
fn filesystem_closes_the_capability_of_an_unparsed_frame() {
    let capability = ram_session();
    let signal = Signal::create().expect("signal to ride the frames");
    for _ in 0..LEAK_FRAMES {
        // Капа приехала в кадре, который не разобран: закрыть её больше некому.
        let rider = signal
            .handle()
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

    // Обе стороны говорят по одной капе, и дескриптор у них общий: разница только в том,
    // что второй не ждёт ответа.
    let abandoned = connect_abandoning(&capability);
    for _ in 0..LEAK_FRAMES {
        // Окно уезжает в ответе, доставить который некому: выданный хэндл
        // остаётся у сервера.
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
        .expect("undelivered backing does not pin");
    kernel_tests::kassert_eq!(grown.count, 1);

    let backing = MemoryRegion::adopt(
        client
            .backing_memory(opened.fd)
            .expect("backing_memory call")
            .expect("backing granted"),
    );
    let capacity = backing.inspect().expect("inspect backing").size_bytes;
    data.copy_in(b"y").expect("payload into data buffer");
    let pinned = client.write(opened.fd, capacity, 1).expect("write call");
    kernel_tests::kassert_eq!(pinned, Err(FileSystemError::NoSpace));
}

fn vends_a_buffer() -> bool {
    let capability = image_session();
    let client = connect(&capability);
    let Ok(Ok(buffer)) = client.acquire_buffer(BUFFER_SIZE) else {
        return false;
    };
    drop(OwnedHandle::adopt(buffer));
    true
}

fn take_and_drop_session() {
    let capability = ram_session();
    let client = connect(&capability);
    let buffer = client
        .acquire_buffer(REAP_BUFFER_SIZE)
        .expect("acquire_buffer call")
        .expect("buffer granted");
    drop(OwnedHandle::adopt(buffer));
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
    // Смерть процесса освобождает его капы: сессия уходит вместе с буфером и дескрипторами, без
    // капы на процесс и без отдельного потока. Цикл сходится только на сервере, убирающем
    // ушедшие сессии: их буферы держат бюджет монтирования.
    for _ in 0..REAP_SESSIONS {
        run_client_fixture();
    }
}

#[kernel_test]
fn filesystem_reaps_the_session_of_a_closed_capability() {
    // Живой клиент закрыл единственную капу значка: для сервера это тот же уход стороны, что и
    // смерть процесса.
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

    // Значок наблюдателя не уходил: его сессия, дескрипторы и буфер на месте.
    let mine = keeper.stat(kept.fd).expect("stat call").expect("stat ok");
    kernel_tests::kassert_eq!(mine.kind, NodeKind::File);
    let occupied = keeper
        .acquire_buffer(BUFFER_SIZE)
        .expect("acquire_buffer call");
    kernel_tests::kassert_eq!(occupied, Err(FileSystemError::AlreadyExists));
    drop(keeper_buffer);
}
