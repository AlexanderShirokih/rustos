//! E2E файловой подсистемы: маршрутизатор имён поднимает процессы файловых систем из образа,
//! выдаёт клиентам сессии с ними, а сами файловые системы работают над своим монтированием.

use alloc::{format, string::String, vec, vec::Vec};

use bootstrap::BootstrapClient;
use filesystem_abi::{
    AccessMode, DirCursor, DirEntry, FileSystemClient, FileSystemError, NAME_MAX,
    NamespaceAdminClient, NamespaceClient, NodeDescriptor, NodeKind, OpenFlags, Opened,
};
use ipc::{
    MessageLen, Transport,
    wire::{HEADER_SIZE, Header, IpcError, MESSAGE_INLINE_MAX, Str, TERMINATOR},
};
use kernel_tests::kernel_test;
use rootkeeper::{
    ManagedComponent, Mount, NamespaceBootError, NamespaceRoots, boot_config,
    launch_namespace as launch_managed_namespace,
};
use runtime::{
    BorrowedHandle, MemoryRegion, OwnedHandle, PortTransport, Process, Resource, Signal, Timeout,
    UserMemFlags, port_send,
};
use spin::Once;
use syscall::Rights;
use userland_image::decode;
use userland_loader::load_entry;

// Конечный срок ответа сервера превращает зависание в диагностируемый Timeout.
const CALL_TIMEOUT_NS: u64 = 2_000_000_000;

const CLIENT_TIMEOUT_NS: u64 = 2_000_000_000;

// Программа-клиент, чья сессия умирает вместе с процессом.
const CLIENT_FIXTURE: &str = "/boot/filesystem-client-fixture";

// Программы файловой подсистемы в образе.
const ROUTER_PROGRAM: &str = "/boot/namespace-router";
const RAM_FILESYSTEM_PROGRAM: &str = "/boot/filesystem-ramfs";

// Фикстура зависшего монтирования: принимает стартовый кадр и замолкает.
const UNRESPONSIVE_FILESYSTEM_PROGRAM: &str = "/boot/unresponsive-filesystem";

// Дерево имён прогона: образ лежит на корне, изменяемая файловая система - под RAM_MOUNT.
const RAM_MOUNT: &str = "/tmp";

// Смещение записи, означающее дозапись в конец.
const APPEND_OFFSET: u64 = u64::MAX;

const BUFFER_SIZE: u32 = 4096;

// Записей в каталоге перечисления: с именами по NAME_MAX они не умещаются в один буфер, и
// обход требует продолжения по DirCursor.
const LISTING_ENTRIES: usize = 70;

// Каталог образа, накрытый корневым монтированием: его записи задаёт манифест образа.
const IMAGE_DIRECTORY: &str = "/boot";

// Файл-данные образа: путь и байты, заданные манифестом образа.
const IMAGE_FILE: &str = "/boot/sample";
const IMAGE_FILE_BODY: &[u8] = b"hello from the userland image\n";

// Файл образа нулевой длины, заданный манифестом образа.
const IMAGE_EMPTY_FILE: &str = "/boot/empty";

// Узел, который заводит клиент, не ждущий ответа.
const ABANDONED_PATH: &str = "/abandoned";

// Кадров в проверке утечки capability: их число обязано превысить ёмкость capability-таблицы
// процесса, иначе утечка не станет видимой.
const LEAK_FRAMES: usize = syscall::HANDLE_TABLE_CAPACITY as usize + 128;

// Сессий в проверке уборки: каждая держит буфер данных на бюджете монтирования, и все они
// вместе его превосходят. Цикл сходится только на сервере, убирающем ушедшие сессии.
const REAP_SESSIONS: usize = 40;

// Буфер сессии в проверке уборки: на потолке буфера бюджет монтирования кончается быстрее.
const REAP_BUFFER_SIZE: u32 = 64 * 1024;

struct NamespacePorts {
    component: ManagedComponent<NamespaceRoots>,
}

static NAMESPACE_PORTS: Once<NamespacePorts> = Once::new();

// Первый вызов поднимает маршрутизатор вместе с деревом имён прогона.
fn router_minter() -> BorrowedHandle<'static> {
    namespace_roots().service_minter()
}

fn router_admin() -> BorrowedHandle<'static> {
    namespace_roots().admin()
}

fn namespace_ports() -> &'static NamespacePorts {
    NAMESPACE_PORTS.call_once(start_namespace)
}

fn namespace_roots() -> &'static NamespaceRoots {
    namespace_ports()
        .component
        .running()
        .expect("namespace router is running")
        .roots()
}

fn namespace_admin_client(capability: BorrowedHandle<'_>) -> NamespaceAdminClient<PortTransport> {
    NamespaceAdminClient::new(PortTransport::client(capability)).with_wait_ns(CALL_TIMEOUT_NS)
}

fn start_namespace() -> NamespacePorts {
    let resource = Resource::self_resource().expect("self metering resource");
    let component = launch_namespace(&resource, rootkeeper::BOOT_MOUNTS)
        .expect("namespace router and boot filesystems started");
    NamespacePorts { component }
}

fn launch_namespace(
    resource: &Resource,
    mounts: &[Mount<'_>],
) -> Result<ManagedComponent<NamespaceRoots>, NamespaceBootError> {
    let bootstrap = BootstrapClient::new(PortTransport::client(crate::bootstrap_handle()));
    let image = MemoryRegion::adopt(acquire_image(&bootstrap));
    let mapping = image.map_full_read_only().expect("map userland image");
    let decoded = decode(mapping.as_bytes()).expect("decode userland image");
    let program = decoded
        .entry(ROUTER_PROGRAM)
        .expect("image carries namespace router")
        .program()
        .expect("namespace router entry is a program");
    let log = crate::log_relay::router_log_session();
    launch_managed_namespace(&program, resource, image, log, boot_config(mounts))
}

fn spawn_from_image(path: &str, start: OwnedHandle) -> Process {
    let bootstrap = BootstrapClient::new(PortTransport::client(crate::bootstrap_handle()));
    let region = MemoryRegion::adopt(acquire_image(&bootstrap));
    let mapping = region.map_full_read_only().expect("map userland image");
    let image = decode(mapping.as_bytes()).expect("decode userland image");
    let program = image
        .entry(path)
        .expect("image carries the entry")
        .program()
        .expect("entry is a program");
    let resource = Resource::self_resource().expect("self metering resource");
    load_entry(&program, &resource, start).expect("spawn from image")
}

fn acquire_image(bootstrap: &BootstrapClient<PortTransport>) -> ipc::wire::Cap {
    bootstrap
        .acquire_userland_image()
        .expect("acquire_userland_image call")
        .expect("image vend")
}

fn namespace_client(capability: BorrowedHandle<'_>) -> NamespaceClient<PortTransport> {
    NamespaceClient::new(PortTransport::client(capability)).with_wait_ns(CALL_TIMEOUT_NS)
}

// Поднимает изменяемое дерево под своим префиксом: у свежего процесса файловой системы пустая
// арена узлов и свой бюджет страниц.
pub(crate) fn mount_ram_filesystem(prefix: &str) -> Result<u32, FileSystemError> {
    namespace_admin_client(router_admin())
        .mount(path(prefix), path(RAM_FILESYSTEM_PROGRAM))
        .expect("mount call")
}

// Снимает монтирование вместе с его процессом.
pub(crate) fn unmount_filesystem(prefix: &str) -> Result<u32, FileSystemError> {
    namespace_admin_client(router_admin())
        .unmount(path(prefix))
        .expect("unmount call")
}

// Значок сессии назначает сама файловая система.
pub(crate) fn session_for(text: &str) -> OwnedHandle {
    let mounted = namespace_client(router_minter())
        .resolve(path(text))
        .expect("resolve call")
        .expect("path is covered by a mount");
    OwnedHandle::adopt(mounted.session)
}

fn ram_session() -> OwnedHandle {
    session_for(RAM_MOUNT)
}

fn image_session() -> OwnedHandle {
    session_for(IMAGE_DIRECTORY)
}

pub(crate) fn connect(capability: &OwnedHandle) -> FileSystemClient<PortTransport> {
    FileSystemClient::new(PortTransport::client(capability)).with_wait_ns(CALL_TIMEOUT_NS)
}

// Порча кадра запроса: чем он отличается от описанного контрактом.
#[derive(Clone, Copy)]
struct Tamper {
    /// Обнулить `txid`: запрос уходит односторонним, и ответить на него некому -
    /// то же положение сервера, что и с ушедшим по тайм-ауту вызывателем.
    one_way: bool,
    /// Сдвинуть ordinal за пределы контракта.
    unknown_ordinal: bool,
    /// Оставить тело без полей: обязательное поле запроса не приедет.
    empty_body: bool,
}

// Клиентский транспорт, портящий кадр запроса перед отправкой.
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

    fn wait_readable(&self, timeout_ns: u64) -> Result<(), IpcError> {
        self.port.wait_readable(timeout_ns)
    }
}

fn connect_tampered(capability: &OwnedHandle, tamper: Tamper) -> FileSystemClient<Tampered> {
    FileSystemClient::new(Tampered {
        port: PortTransport::client(capability),
        tamper,
    })
    .with_wait_ns(CALL_TIMEOUT_NS)
}

// Ответ этому клиенту доставить некому: port_reply сервера отказывает так же, как на ушедшем
// по тайм-ауту вызывателе.
fn connect_abandoning(capability: &OwnedHandle) -> FileSystemClient<Tampered> {
    connect_tampered(
        capability,
        Tamper {
            one_way: true,
            ..Tamper::NONE
        },
    )
}

// Маппинг буфера живёт, пока жива его капа.
fn acquire_buffer(client: &FileSystemClient<PortTransport>) -> MemoryRegion {
    let cap = client
        .acquire_buffer(BUFFER_SIZE)
        .expect("acquire_buffer call")
        .expect("buffer granted");
    MemoryRegion::adopt(cap)
}

pub(crate) fn path(text: &str) -> Str<'_, { filesystem_abi::PATH_MAX }> {
    Str::new(text).expect("path within bound")
}

fn open(
    client: &FileSystemClient<PortTransport>,
    text: &str,
    mode: AccessMode,
    flags: OpenFlags,
) -> Opened {
    client
        .open(path(text), mode, flags)
        .expect("open call")
        .expect("open ok")
}

fn write_all(
    client: &FileSystemClient<PortTransport>,
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

// Страницы буфера отображены и у сервера: проверяется их приватная копия.
fn buffer_head(buffer: &runtime::Mapping, len: usize) -> Vec<u8> {
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

// Имя записи длиной NAME_MAX, уникальное по индексу.
fn entry_name(index: usize) -> String {
    format!("{index:0>NAME_MAX$}")
}

// Вызов list повторяется с выданной позиции, пока перечисление не дойдёт до конца.
fn walk_directory(
    client: &FileSystemClient<PortTransport>,
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
        client
            .close(entry.fd)
            .expect("close call")
            .expect("close ok");
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
    client
        .close(opened.fd)
        .expect("close call")
        .expect("close ok");
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

    // Корень накрывает всё: путь под ним уходит файловой системе целиком.
    let image = namespace
        .resolve(path(IMAGE_FILE))
        .expect("resolve call")
        .expect("root mount covers the path");
    kernel_tests::kassert_eq!(image.prefix_len, 0);
    drop(OwnedHandle::adopt(image.session));

    // Более длинный префикс выигрывает у корня, и его длину клиент снимает с пути сам.
    let ram = namespace
        .resolve(path("/tmp/inner"))
        .expect("resolve call")
        .expect("ram mount covers the path");
    kernel_tests::kassert_eq!(ram.prefix_len, RAM_MOUNT.len() as u32);
    drop(OwnedHandle::adopt(ram.session));
}

#[kernel_test]
fn namespace_refuses_an_occupied_prefix_and_an_unknown_program() {
    let namespace = namespace_admin_client(router_admin());

    let occupied = namespace
        .mount(path(RAM_MOUNT), path(RAM_FILESYSTEM_PROGRAM))
        .expect("mount call");
    kernel_tests::kassert_eq!(occupied, Err(FileSystemError::AlreadyExists));

    let absent = namespace
        .mount(path("/absent-mount"), path("/boot/no-such-program"))
        .expect("mount call");
    kernel_tests::kassert_eq!(absent, Err(FileSystemError::NotFound));

    // Отказ монтирования не занял префикс: он остаётся свободным.
    let unmounted = namespace
        .unmount(path("/absent-mount"))
        .expect("unmount call");
    kernel_tests::kassert_eq!(unmounted, Err(FileSystemError::NotFound));
}

#[kernel_test]
fn namespace_rejects_admin_operations_on_the_service_port() {
    // Управление таблицей монтирования живёт на admin-порту: его ordinal сервисный порт не
    // разбирает, и кадр отвергается целиком, не доходя до операции.
    let admin_on_service = namespace_admin_client(router_minter());
    let refused = admin_on_service.unmount(path("/"));
    kernel_tests::kassert_eq!(refused, Err(IpcError::PeerClosed));

    // Отвергнутый кадр не тронул ни таблицу, ни цикл обслуживания: корень резолвится дальше.
    let mounted = namespace_client(router_minter())
        .resolve(path(IMAGE_FILE))
        .expect("resolve call")
        .expect("root mount survives the rejected frame");
    drop(OwnedHandle::adopt(mounted.session));
}

#[kernel_test]
fn namespace_reports_an_unresponsive_mount() {
    const STUCK_MOUNT: &str = "/stuck";

    let namespace = namespace_admin_client(router_admin());
    namespace
        .mount(path(STUCK_MOUNT), path(UNRESPONSIVE_FILESYSTEM_PROGRAM))
        .expect("mount call")
        .expect("fixture mounted");

    // Файловая система приняла стартовый кадр и замолчала: `bind` не отвечает, и маршрутизатор
    // обязан отпустить вызов по своему сроку, отличив зависшее монтирование от отсутствующего.
    let resolved = namespace_client(router_minter())
        .resolve(path(STUCK_MOUNT))
        .expect("resolve call");
    kernel_tests::kassert_eq!(resolved, Err(FileSystemError::Unresponsive));

    // Снятие убирает зависший процесс, и путь снова накрывает корневое монтирование.
    namespace
        .unmount(path(STUCK_MOUNT))
        .expect("unmount call")
        .expect("fixture unmounted");
    let rerouted = namespace_client(router_minter())
        .resolve(path(STUCK_MOUNT))
        .expect("resolve call")
        .expect("root mount covers the path again");
    kernel_tests::kassert_eq!(rerouted.prefix_len, 0);
    drop(OwnedHandle::adopt(rerouted.session));
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

    // Состав образа задаёт манифест: перечисление несёт и программы, и
    // файл-данные.
    for expected in ["testrunner", "namespace-router", "sample"] {
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
fn filesystem_has_no_backing_window_for_a_ramfs_file() {
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
        .unlink(path("/boot/sample/inner"))
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
    client
        .close(stale.fd)
        .expect("close call")
        .expect("close ok");
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

// Кадр сервер не разберёт: ordinal вне контракта, тело пустое, txid == 0 оставляет его без
// ответа. Владение capability кадр переносит в таблицу сервера. Отправка идёт сырым syscall:
// транспорт сводит любой отказ к PeerClosed, а сроком отказ сервера отличается от зависания.
fn send_unparsed_frame_with_capability(session: &OwnedHandle, capability: OwnedHandle) {
    let mut frame = [0u8; MESSAGE_INLINE_MAX];
    Header::new(u64::MAX, 0, 0)
        .encode(&mut frame)
        .expect("frame header");
    frame[HEADER_SIZE] = TERMINATOR;
    crate::ipc_raw::write_frame(&frame[..=HEADER_SIZE], &[capability.into_raw().raw()]);
    kernel_tests::kassert_eq!(
        port_send(session.as_raw(), Timeout::from_ns(CALL_TIMEOUT_NS).raw()),
        0
    );
}

#[kernel_test]
fn filesystem_closes_the_capability_of_an_unparsed_frame() {
    let capability = ram_session();
    let signal = Signal::create().expect("signal to ride the frames");
    for _ in 0..LEAK_FRAMES {
        // Капа приехала в кадре, который не разобран: закрыть её больше некому.
        let rider = signal
            .handle()
            .duplicate(Rights::READ | Rights::TRANSFER, 0)
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

// Выдаёт ли сервер capability новому клиенту: заполненную утечкой таблицу выдаёт отказ.
fn vends_a_buffer() -> bool {
    let capability = image_session();
    let client = connect(&capability);
    let Ok(Ok(buffer)) = client.acquire_buffer(BUFFER_SIZE) else {
        return false;
    };
    drop(OwnedHandle::adopt(buffer));
    true
}

// Значок сессии назначает сервер: снаружи её видно только по бюджету монтирования, который
// держит её буфер.
fn take_and_drop_session() {
    let capability = ram_session();
    let client = connect(&capability);
    let buffer = client
        .acquire_buffer(REAP_BUFFER_SIZE)
        .expect("acquire_buffer call")
        .expect("buffer granted");
    drop(OwnedHandle::adopt(buffer));
}

// Капу сессии фикстура держит одна: её смерть равна уходу значка.
fn run_client_fixture() {
    let client = spawn_from_image(CLIENT_FIXTURE, ram_session());
    client
        .join(Timeout::from_ns(CLIENT_TIMEOUT_NS))
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
