//! E2E файловой подсистемы на композиции плана testrunner.

use alloc::{format, string::String, vec, vec::Vec};

use filesystem_abi::{
    APPEND_OFFSET, AccessMode, DirCursor, DirEntry, FileSystemError, NAME_MAX, NodeDescriptor,
    NodeKind, OpenFlags, Opened, Path, file_system, file_system_mount, namespace,
};
use ipc::{
    Channel, Incoming, Outgoing, Unsent,
    wire::{
        FieldCursor, HEADER_SIZE, Header, IpcError, MESSAGE_INLINE_MAX, MessageBuf, Str, TERMINATOR,
    },
};
use kernel_objects::{
    Delivery, MemoryRegion, OwnedHandle, Port, Resource, Session, SessionTransport, Signal,
};
use kernel_tests::kernel_test;
use rootkeeper::relay::{ComponentNames, LogSources};
use runtime::{PortTransport, Priority, Timeout, UserMemFlags, port_send, spawn_owned};
use spin::Once;
use syscall::Rights;
use userland::USERLAND_PAGE_SIZE;
use userland_image::ExportIndex;

use crate::{
    composition::{BOOT_PLAN_ENTRY, Composition, Importer, Mounted, admin, image, pause, view},
    fixture::{JOIN_TIMEOUT, spawn_entry},
    log_relay::SharedLogSessions,
};

const FILE_SYSTEM_REPLY_TIMEOUT: Timeout = Timeout::from_secs(2);

const NAMESPACE_REPLY_TIMEOUT: Timeout = Timeout::from_secs(5);
const MOUNT_END_TIMEOUT: Timeout = Timeout::from_secs(5);
const OUT_OF_SCOPE_RESOLVE_TIMEOUT: Timeout = Timeout::from_millis(100);

const CLIENT_FIXTURE: &str = "/filesystem-client-fixture/exec";

const RAM_MOUNT: &str = "/tmp";

const IMAGE_MOUNT: &str = "/boot";

const SHADOW_MOUNT: &str = "/boot/testrunner";

const ROOT_VIEW: &str = "root";
const RAM_VIEW: &str = "tmp";
const SPARE_VIEW: &str = "spare";
const STUCK_VIEW: &str = "stuck";
const UNCOVERED_VIEW: &str = "uncovered";
const SHADOW_VIEW: &str = "shadow";
const QUEUE_VIEW: &str = "queue";
const EMPTY_SCOPE_ADMIN: &str = "empty-scope-admin";

pub(crate) const STRESS_VIEW: &str = "stress";
pub(crate) const FAILING_VIEW: &str = "failing";

const NAMESPACE_REQUIREMENTS: &[crate::composition::Import] = &[
    view(ROOT_VIEW, ""),
    view(RAM_VIEW, RAM_MOUNT),
    view(SPARE_VIEW, "/spare"),
    view(STUCK_VIEW, "/stuck"),
    view(UNCOVERED_VIEW, "/uncovered"),
    view("grafted", "/grafted"),
    admin("grafted-admin"),
    view("paired-second", "/paired-second"),
    admin("paired-second-admin"),
    view("paired-detached", "/paired-detached"),
    admin("paired-detached-admin"),
    view(SHADOW_VIEW, SHADOW_MOUNT),
    admin("shadow-admin"),
    view("replaced", "/replaced"),
    admin("replaced-admin"),
    view(STRESS_VIEW, "/stress"),
    admin("stress-admin"),
    view("other", "/other"),
    admin("other-admin"),
    view(QUEUE_VIEW, "/queue"),
    admin("queue-admin"),
    view(FAILING_VIEW, "/failing"),
    admin("failing-admin"),
    admin(EMPTY_SCOPE_ADMIN),
];

pub(crate) const BUFFER_SIZE: u32 = 4096;

const LISTING_ENTRIES: usize = 70;

pub(crate) const COMPONENT_DIRECTORY: &str = "/testrunner";

pub(crate) const IMAGE_FILE: &str = "/testrunner/sample";
pub(crate) const IMAGE_FILE_BODY: &[u8] = b"hello from the userland image\n";

const IMAGE_EMPTY_FILE: &str = "/testrunner/empty";

const UNANSWERED_PATH: &str = "/unanswered";

const LEAK_FRAMES: usize = syscall::HANDLE_TABLE_CAPACITY as usize + 128;

const REAP_SESSIONS: usize = 40;

const REAP_BUFFER_SIZE: u32 = 64 * 1024;

struct Namespace {
    importer: Importer,
    _composition: Composition,
}

static NAMESPACE: Once<Namespace> = Once::new();

fn importer() -> &'static Importer {
    &NAMESPACE.call_once(start_namespace).importer
}

fn start_namespace() -> Namespace {
    let plan = image()
        .plan(BOOT_PLAN_ENTRY)
        .expect("compiled boot plan decodes");
    let composition = Composition::start(
        plan,
        ExportIndex::EMPTY,
        LogSources::new(ComponentNames::of(&plan), SharedLogSessions),
    );
    let importer = composition.importer(NAMESPACE_REQUIREMENTS);
    for (requirement, text) in [(ROOT_VIEW, IMAGE_MOUNT), (RAM_VIEW, Path::ROOT.as_str())] {
        drop(resolved(&importer, requirement, text));
    }
    Namespace {
        importer,
        _composition: composition,
    }
}

fn view_of(requirement: &str) -> Port {
    importer().port::<namespace::Protocol>(requirement)
}

fn resolved(importer: &Importer, requirement: &str, text: &str) -> Session<file_system::Protocol> {
    let view = importer.port::<namespace::Protocol>(requirement);
    let session = namespace_client(&view)
        .resolve(path(text))
        .expect("resolve call")
        .expect("path is covered by a mount");
    Session::from_session(session)
}

fn ram_port() -> Port {
    Port::from(ram_session().into_handle())
}

fn namespace_client(capability: &Port) -> namespace::Client<PortTransport> {
    namespace::Client::new(PortTransport::new(capability)).with_wait(NAMESPACE_REPLY_TIMEOUT)
}

pub(crate) fn mount_ram_window(requirement: &str, scope: &str) -> Mounted {
    let client = connect(ram_session());
    close(
        &client,
        &open(
            &client,
            scope,
            AccessMode::Read,
            OpenFlags::CREATE | OpenFlags::DIRECTORY,
        ),
    );
    let window = client
        .open_root(path(scope))
        .expect("open_root call")
        .expect("window granted");
    Mounted::new(importer(), requirement, Session::from_session(window))
}

pub(crate) fn mount(requirement: &str, filesystem: Session<file_system::Protocol>) -> Mounted {
    Mounted::new(importer(), requirement, filesystem)
}

pub(crate) fn root_view() -> Port {
    view_of(ROOT_VIEW)
}

pub(crate) fn remove_ram_directory(text: &str) {
    connect(ram_session())
        .unlink(path(text), NodeKind::Directory)
        .expect("unlink call")
        .expect("unlink of an emptied directory");
}

pub(crate) fn session_for(text: &str) -> Session<file_system::Protocol> {
    resolved(importer(), ROOT_VIEW, text)
}

pub(crate) fn ram_session() -> Session<file_system::Protocol> {
    resolved(importer(), RAM_VIEW, Path::ROOT.as_str())
}

pub(crate) fn image_session() -> Session<file_system::Protocol> {
    session_for(IMAGE_MOUNT)
}

pub(crate) fn connect(
    session: Session<file_system::Protocol>,
) -> file_system::Client<SessionTransport> {
    session.client().with_wait(FILE_SYSTEM_REPLY_TIMEOUT)
}

#[derive(Clone, Copy)]
struct Tamper {
    one_way: bool,
    unknown_ordinal: bool,
    empty_body: bool,
}

struct Tampered {
    port: SessionTransport,
    tamper: Tamper,
}

impl Tamper {
    const NONE: Self = Self {
        one_way: false,
        unknown_ordinal: false,
        empty_body: false,
    };

    fn apply(self, frame: &Outgoing<'_, Delivery>) -> Result<MessageBuf<Delivery>, IpcError> {
        if frame.resources().has_capabilities() {
            return Err(IpcError::TooManyHandles);
        }
        let parts = Header::split(frame.as_bytes())?;
        let mut header = parts.header;
        if self.unknown_ordinal {
            header.ordinal ^= 1;
        }
        let mut out = MessageBuf::new();
        out.write_header(&header)?;
        if !self.empty_body {
            let mut fields = FieldCursor::new(parts.body);
            while let Some(field) = fields.next_field()? {
                out.write_field(field.id, field.data)?;
            }
        }
        out.finish()?;
        Ok(out)
    }
}

impl Channel for Tampered {
    type Resources = Delivery;

    fn send<'b>(
        &self,
        frame: Outgoing<'b, Delivery>,
        wait: Timeout,
    ) -> Result<(), Unsent<'b, Delivery>> {
        let mut tampered = match self.tamper.apply(&frame) {
            Ok(tampered) => tampered,
            Err(error) => return Err((error, frame)),
        };
        self.port
            .send(tampered.outgoing(), wait)
            .map_err(|(error, _)| (error, frame))
    }

    fn call<'b>(
        &self,
        frame: Outgoing<'b, Delivery>,
        wait: Timeout,
    ) -> Result<Incoming<'b, Delivery>, IpcError> {
        let mut tampered = self.tamper.apply(&frame)?;
        if self.tamper.one_way {
            self.port
                .send(tampered.outgoing(), wait)
                .map_err(|(error, _)| error)?;
            return Err(IpcError::PeerClosed);
        }
        let Incoming { bytes, resources } = self.port.call(tampered.outgoing(), wait)?;
        let answer = frame
            .into_parts()
            .buffer
            .get_mut(..bytes.len())
            .ok_or(IpcError::FrameOverflow)?;
        answer.copy_from_slice(bytes);
        Ok(Incoming {
            bytes: answer,
            resources,
        })
    }
}

fn connect_tampered(
    client: file_system::Client<SessionTransport>,
    tamper: Tamper,
) -> file_system::Client<Tampered> {
    file_system::Client::new(Tampered {
        port: client.into_channel(),
        tamper,
    })
    .with_wait(FILE_SYSTEM_REPLY_TIMEOUT)
}

fn connect_without_reply(
    client: file_system::Client<SessionTransport>,
) -> file_system::Client<Tampered> {
    connect_tampered(
        client,
        Tamper {
            one_way: true,
            ..Tamper::NONE
        },
    )
}

pub(crate) fn acquire_buffer(client: &file_system::Client<SessionTransport>) -> MemoryRegion {
    let cap = client
        .acquire_buffer(BUFFER_SIZE)
        .expect("acquire_buffer call")
        .expect("buffer granted");
    MemoryRegion::from(cap)
}

pub(crate) fn path(text: &str) -> Str<'_, { filesystem_abi::PATH_MAX }> {
    Str::new(text).expect("path within bound")
}

pub(crate) fn open(
    client: &file_system::Client<SessionTransport>,
    text: &str,
    mode: AccessMode,
    flags: OpenFlags,
) -> Opened {
    client
        .open(path(text), mode, flags)
        .expect("open call")
        .expect("open ok")
}

fn close(client: &file_system::Client<SessionTransport>, opened: &Opened) {
    client
        .close(opened.fd)
        .expect("close call")
        .expect("close ok");
}

pub(crate) fn write_all(
    client: &file_system::Client<SessionTransport>,
    buffer: &kernel_objects::Mapping,
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
    client: &file_system::Client<SessionTransport>,
    buffer: &kernel_objects::Mapping,
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
        kernel_tests::kassert!(listed.next.raw() > from.raw());
        from = listed.next;
    }
}

#[kernel_test]
fn filesystem_creates_and_writes_a_file() {
    let client = connect(ram_session());
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
    let client = connect(ram_session());
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
    let client = connect(ram_session());
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
    let client = connect(ram_session());
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
    let client = connect(ram_session());

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
        .expect("unlink ok");

    let reopened = client
        .open(path("/removed"), AccessMode::Read, OpenFlags::NONE)
        .expect("open call");
    kernel_tests::kassert_eq!(reopened, Err(FileSystemError::NotFound));
    let again = client
        .unlink(path("/removed"), NodeKind::File)
        .expect("unlink call");
    kernel_tests::kassert_eq!(again, Err(FileSystemError::NotFound));
}

#[kernel_test]
fn filesystem_rejects_write_through_a_read_only_descriptor() {
    let client = connect(ram_session());
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

    let truncated = client
        .open(path("/read-only"), AccessMode::Read, OpenFlags::TRUNCATE)
        .expect("open call");
    kernel_tests::kassert_eq!(truncated, Err(FileSystemError::WrongMode));
}

#[kernel_test]
fn a_view_does_not_resolve_outside_its_scope() {
    let uncovered = view_of(UNCOVERED_VIEW);
    let outside = namespace::Client::new(PortTransport::new(&uncovered))
        .with_wait(OUT_OF_SCOPE_RESOLVE_TIMEOUT)
        .resolve(path(Path::ROOT.as_str()));
    kernel_tests::kassert_eq!(outside.err(), Some(IpcError::Timeout));
}

#[kernel_test]
fn a_view_counts_paths_and_the_pair_root_from_its_scope() {
    let ram = connect(ram_session());
    close(
        &ram,
        &open(
            &ram,
            "/a",
            AccessMode::Read,
            OpenFlags::CREATE | OpenFlags::DIRECTORY,
        ),
    );

    let pair = namespace_client(&view_of(RAM_VIEW))
        .resolve_pair(path("/a/x"), path("/a/y"))
        .expect("resolve_pair call")
        .expect("the mount of the scope covers the pair");
    kernel_tests::kassert_eq!(pair.root_len, 2);
}

#[kernel_test]
fn a_session_mounts_once() {
    let mounted = Mounted::new(importer(), "other", ram_session());
    let again = mounted.mount(ram_session()).expect("mount call");
    kernel_tests::kassert_eq!(again, Err(FileSystemError::AlreadyExists));
}

const REPLACED_VIEW: &str = "replaced";
const REPLACED_ENTRY: &str = "/replaced-probe";

fn replaced_entry_kind() -> NodeKind {
    let window = resolved(importer(), REPLACED_VIEW, Path::ROOT.as_str());
    open(
        &connect(window),
        REPLACED_ENTRY,
        AccessMode::Read,
        OpenFlags::NONE,
    )
    .kind
}

#[kernel_test]
fn a_mount_of_a_new_session_replaces_the_one_of_the_old() {
    let ram = connect(ram_session());
    close(
        &ram,
        &open(
            &ram,
            REPLACED_ENTRY,
            AccessMode::ReadWrite,
            OpenFlags::CREATE,
        ),
    );
    let first = Mounted::new(importer(), REPLACED_VIEW, image_session());
    let _second = Mounted::new(importer(), REPLACED_VIEW, ram_session());

    kernel_tests::kassert!(first.ended(MOUNT_END_TIMEOUT));
    kernel_tests::kassert_eq!(replaced_entry_kind(), NodeKind::File);
    drop(first);
    kernel_tests::kassert_eq!(replaced_entry_kind(), NodeKind::File);
}

#[kernel_test]
fn namespace_resolves_each_path_to_the_longest_prefix() {
    let capability = view_of(ROOT_VIEW);
    let namespace = namespace_client(&capability);

    let image = namespace
        .resolve(path(IMAGE_MOUNT))
        .expect("resolve call")
        .expect("image mount covers the path");
    drop(image);

    connect(ram_session())
        .open(
            path("/inner"),
            AccessMode::ReadWrite,
            OpenFlags::CREATE | OpenFlags::DIRECTORY,
        )
        .expect("create directory call")
        .expect("directory created");

    let ram = namespace
        .resolve(path("/tmp/inner"))
        .expect("resolve call")
        .expect("ram mount covers the path");
    drop(ram);
}

#[kernel_test]
fn namespace_grafts_the_session_it_is_handed() {
    const GRAFT_VIEW: &str = "grafted";
    const GRAFT_MOUNT: &str = "/grafted";
    const GRAFT_ENTRY: &str = "/grafted-probe";

    open(
        &connect(Session::from_handle(ram_port())),
        GRAFT_ENTRY,
        AccessMode::ReadWrite,
        OpenFlags::CREATE,
    );
    let _grafted = Mounted::new(importer(), GRAFT_VIEW, Session::from_handle(ram_port()));

    let opened = open(
        &connect(session_for(GRAFT_MOUNT)),
        GRAFT_ENTRY,
        AccessMode::Read,
        OpenFlags::NONE,
    );
    kernel_tests::kassert_eq!(opened.kind, NodeKind::File);

    let absent = connect(image_session())
        .open(path(GRAFT_ENTRY), AccessMode::Read, OpenFlags::NONE)
        .expect("open call");
    kernel_tests::kassert_eq!(absent, Err(FileSystemError::NotFound));
}

#[kernel_test]
fn namespace_rejects_admin_operations_on_the_service_port() {
    let capability = view_of(ROOT_VIEW);
    let refused = file_system_mount::Client::new(PortTransport::new(&capability))
        .with_wait(FILE_SYSTEM_REPLY_TIMEOUT)
        .mount(path("/invalid"), Session::from_handle(ram_port()));
    kernel_tests::kassert_eq!(refused.err(), Some(IpcError::PeerClosed));

    let mounted = namespace_client(&view_of(ROOT_VIEW))
        .resolve(path(IMAGE_MOUNT))
        .expect("resolve call")
        .expect("image mount survives the rejected frame");
    drop(mounted);
}

#[kernel_test]
fn namespace_reports_an_unresponsive_mount() {
    let resolved = namespace_client(&view_of(STUCK_VIEW))
        .resolve(path(Path::ROOT.as_str()))
        .expect("resolve call");
    kernel_tests::kassert_eq!(resolved.err(), Some(FileSystemError::Unresponsive));
}

#[kernel_test]
fn namespace_resolves_a_pair_to_the_session_of_their_common_directory() {
    let ram = connect(ram_session());
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

    let capability = view_of(ROOT_VIEW);
    let namespace = namespace_client(&capability);
    let pair = namespace
        .resolve_pair(path("/tmp/pair-left/first"), path("/tmp/pair-right/second"))
        .expect("resolve_pair call")
        .expect("one mount covers the pair");
    kernel_tests::kassert_eq!(pair.root_len, RAM_MOUNT.len() as u32);
    let opened = open(
        &connect(Session::from_session(pair.session)),
        "/pair-left/first",
        AccessMode::Read,
        OpenFlags::NONE,
    );
    kernel_tests::kassert_eq!(opened.kind, NodeKind::File);
}

#[kernel_test]
fn namespace_keeps_the_pair_scope_inside_the_mount_root() {
    let pair = namespace_client(&view_of(ROOT_VIEW))
        .resolve_pair(path(RAM_MOUNT), path("/tmp/inside"))
        .expect("resolve_pair call")
        .expect("one mount covers the pair");
    kernel_tests::kassert_eq!(pair.root_len, RAM_MOUNT.len() as u32);
}

#[kernel_test]
fn namespace_reports_a_pair_under_different_mounts() {
    let ram = connect(ram_session());
    close(
        &ram,
        &open(&ram, "/second", AccessMode::ReadWrite, OpenFlags::CREATE),
    );
    let _second = Mounted::new(importer(), "paired-second", ram_session());
    let capability = view_of(ROOT_VIEW);
    let namespace = namespace_client(&capability);

    let across = namespace
        .resolve_pair(path("/tmp/first"), path("/paired-second/second"))
        .expect("resolve_pair call")
        .map(|pair| pair.root_len);
    kernel_tests::kassert_eq!(across, Err(FileSystemError::CrossMount));

    drop(
        namespace
            .resolve(path("/paired-second/second"))
            .expect("resolve call")
            .expect("second mount covers the path"),
    );
}

#[kernel_test]
fn namespace_opens_no_session_for_a_call_without_a_reply_channel() {
    let capability = view_of(ROOT_VIEW);
    let without_reply = namespace::Client::new(Tampered {
        port: SessionTransport::new(capability),
        tamper: Tamper {
            one_way: true,
            ..Tamper::NONE
        },
    })
    .with_wait(FILE_SYSTEM_REPLY_TIMEOUT);
    for _ in 0..LEAK_FRAMES {
        let _ = without_reply.resolve_pair(path("/tmp/first"), path("/tmp/second"));
    }

    let covered = namespace_client(&view_of(ROOT_VIEW))
        .resolve(path(RAM_MOUNT))
        .expect("resolve call");
    kernel_tests::kassert!(covered.is_ok());
    let client = connect(ram_session());
    close(
        &client,
        &open(
            &client,
            "/unanswered-pair",
            AccessMode::ReadWrite,
            OpenFlags::CREATE,
        ),
    );
}

#[kernel_test]
fn namespace_pair_scope_survives_a_table_change() {
    let mounted = Mounted::new(importer(), "paired-detached", ram_session());
    let pair = namespace_client(&view_of(ROOT_VIEW))
        .resolve_pair(
            path("/paired-detached/first"),
            path("/paired-detached/second"),
        )
        .expect("resolve_pair call")
        .expect("fixture covers the pair");
    let scope = Session::from_session(pair.session);

    drop(mounted);

    let client = connect(scope);
    close(
        &client,
        &open(&client, "/first", AccessMode::ReadWrite, OpenFlags::CREATE),
    );
    client
        .rename(path("/first"), path("/second"))
        .expect("rename call")
        .expect("rename inside the issued scope");
}

const ROUTER_VIEW_WAITING_REQUESTS_MAX: usize = 8;
const QUEUED_RESOLVE_TIMEOUT: Timeout = Timeout::from_secs(20);
const QUEUE_CALLER_STACK_SIZE: u64 = 0x4000;
const EVICTION_POLL_INTERVAL: Timeout = Timeout::from_millis(10);
const EVICTION_POLL_ATTEMPTS: usize = 500;
const QUEUE_CALLER_ANSWERED: u32 = 0;
const QUEUE_CALLER_EVICTED: u32 = 1;
const QUEUE_CALLER_FAILED: u32 = 2;

#[kernel_test]
fn a_full_view_queue_evicts_one_request_and_keeps_the_rest() {
    let capability = view_of(QUEUE_VIEW);
    let raw = capability.as_raw();
    let mut callers: Vec<_> = (0..=ROUTER_VIEW_WAITING_REQUESTS_MAX)
        .map(|_| {
            spawn_owned(
                move || {
                    let client = namespace::Client::new(PortTransport::new(raw))
                        .with_wait(QUEUED_RESOLVE_TIMEOUT);
                    match client.resolve(path(Path::ROOT.as_str())) {
                        Ok(Ok(_)) => QUEUE_CALLER_ANSWERED,
                        Ok(Err(FileSystemError::NoSpace)) => QUEUE_CALLER_EVICTED,
                        _ => QUEUE_CALLER_FAILED,
                    }
                },
                QUEUE_CALLER_STACK_SIZE,
                Priority::new(1),
            )
            .expect("queue caller starts")
        })
        .collect();

    let evicted = (0..EVICTION_POLL_ATTEMPTS)
        .find_map(|_| {
            let ended = callers
                .iter()
                .position(|caller| caller.wait(Timeout::POLL).is_ok());
            if ended.is_none() {
                pause(EVICTION_POLL_INTERVAL);
            }
            ended
        })
        .expect("a queue caller ends before the mount");
    let evicted = callers.swap_remove(evicted);
    kernel_tests::kassert_eq!(evicted.exit_code(), Ok(QUEUE_CALLER_EVICTED));
    kernel_tests::kassert!(evicted.join(Timeout::POLL).is_ok());

    let mounted = Mounted::new(importer(), QUEUE_VIEW, ram_session());
    for caller in callers {
        kernel_tests::kassert!(caller.wait(JOIN_TIMEOUT).is_ok());
        kernel_tests::kassert_eq!(caller.exit_code(), Ok(QUEUE_CALLER_ANSWERED));
        kernel_tests::kassert!(caller.join(Timeout::POLL).is_ok());
    }
    drop(mounted);
}

#[kernel_test]
fn a_mount_with_an_empty_path_is_refused() {
    let port = importer().port::<file_system_mount::Protocol>(EMPTY_SCOPE_ADMIN);
    let answer = file_system_mount::Client::new(PortTransport::new(&port))
        .with_wait(FILE_SYSTEM_REPLY_TIMEOUT)
        .mount(Str::new("").expect("empty fits"), ram_session());
    kernel_tests::kassert_eq!(answer, Ok(Err(FileSystemError::InvalidPath)));
}

#[kernel_test]
fn a_closed_admin_session_ends_its_mount_and_keeps_issued_windows() {
    let mut mounted = Mounted::new(importer(), SHADOW_VIEW, ram_session());
    let window = resolved(importer(), SHADOW_VIEW, Path::ROOT.as_str());
    mounted.close_admin();
    kernel_tests::kassert!(mounted.ended(MOUNT_END_TIMEOUT));
    drop(mounted);

    let opened = connect(window)
        .open(path(Path::ROOT.as_str()), AccessMode::Read, OpenFlags::NONE)
        .expect("issued window still answers")
        .expect("issued window keeps its root");
    kernel_tests::kassert_eq!(opened.kind, NodeKind::Directory);
    let rerouted = session_for(SHADOW_MOUNT);
    let image_file = open(
        &connect(rerouted),
        "/sample",
        AccessMode::Read,
        OpenFlags::NONE,
    );
    kernel_tests::kassert_eq!(image_file.kind, NodeKind::File);
}

#[kernel_test]
fn filesystem_reads_a_file_from_the_image() {
    let client = connect(image_session());
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
    let client = connect(image_session());
    let region = acquire_buffer(&client);
    let buffer = region
        .map_full(UserMemFlags::ReadWrite)
        .expect("map data buffer");

    let directory = open(
        &client,
        Path::ROOT.as_str(),
        AccessMode::Read,
        OpenFlags::DIRECTORY,
    );
    kernel_tests::kassert_eq!(directory.kind, NodeKind::Directory);

    let names = walk_directory(&client, &buffer, directory.fd);

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
    for expected in ["exec", "sample", "empty"] {
        kernel_tests::kassert!(names.iter().any(|name| name.as_str() == expected));
    }
}

#[kernel_test]
fn filesystem_mounts_hold_separate_trees() {
    let ram = connect(ram_session());
    let image = connect(image_session());

    open(
        &ram,
        "/only-in-ram",
        AccessMode::ReadWrite,
        OpenFlags::CREATE,
    );

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
    let client = connect(image_session());
    let region = acquire_buffer(&client);
    let buffer = region
        .map_full(UserMemFlags::ReadWrite)
        .expect("map data buffer");

    let opened = open(&client, IMAGE_FILE, AccessMode::Read, OpenFlags::NONE);
    let count = client
        .read(opened.fd, 0, BUFFER_SIZE)
        .expect("read call")
        .expect("read ok");

    let window = MemoryRegion::from(
        client
            .backing_memory(opened.fd)
            .expect("backing_memory call")
            .expect("window granted"),
    );
    let mapped = window
        .map_full(UserMemFlags::ReadOnly)
        .expect("map backing window");
    kernel_tests::kassert!(mapped.size_bytes() >= opened.size);
    kernel_tests::kassert_eq!(
        buffer_head(&mapped, opened.size as usize),
        buffer_head(&buffer, count as usize)
    );

    kernel_tests::kassert!(window.map_full(UserMemFlags::ReadWrite).is_err());
}

#[kernel_test]
fn filesystem_serves_an_empty_file_from_the_image() {
    let client = connect(image_session());
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

    let refused = client
        .backing_memory(opened.fd)
        .expect("backing_memory call");
    kernel_tests::kassert_eq!(refused.err(), Some(FileSystemError::NotSupported));
}

#[kernel_test]
fn filesystem_has_no_backing_window_for_an_empty_ramfs_file() {
    let client = connect(ram_session());

    let opened = open(
        &client,
        "/on-the-heap",
        AccessMode::ReadWrite,
        OpenFlags::CREATE,
    );
    let refused = client
        .backing_memory(opened.fd)
        .expect("backing_memory call");
    kernel_tests::kassert_eq!(refused.err(), Some(FileSystemError::NotSupported));
}

#[kernel_test]
fn filesystem_ramfs_backing_is_shared_and_pins_capacity() {
    let client = connect(ram_session());
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

    let backing = MemoryRegion::from(
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
    let client = connect(ram_session());
    let data_region = acquire_buffer(&client);
    let data = data_region
        .map_full(UserMemFlags::ReadWrite)
        .expect("map data buffer");
    let opened = open(&client, "/sparse", AccessMode::ReadWrite, OpenFlags::CREATE);
    write_all(&client, &data, &opened, 0, b"head");

    let backing = MemoryRegion::from(
        client
            .backing_memory(opened.fd)
            .expect("backing_memory call")
            .expect("backing granted"),
    );
    let mapped = backing
        .map_full(UserMemFlags::ReadWrite)
        .expect("map ramfs backing");
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
    let creator = connect(ram_session());
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
        .expect("create backed file");

    let reader = connect(ram_session());
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

    let shared = MemoryRegion::from(
        reader
            .backing_memory(opened.fd)
            .expect("backing_memory call")
            .expect("backing granted"),
    );
    kernel_tests::kassert!(
        shared
            .duplicate(Rights::READ | Rights::TRANSFER, None)
            .is_err()
    );
}

fn create_filler_tree(
    client: &file_system::Client<SessionTransport>,
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
    client: &file_system::Client<SessionTransport>,
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

fn buffered_sessions(
    requirement: &str,
    count: usize,
) -> Vec<file_system::Client<SessionTransport>> {
    (0..count)
        .map(|_| {
            let client = connect(resolved(importer(), requirement, Path::ROOT.as_str()));
            let granted = client
                .acquire_buffer(BUFFER_SIZE)
                .expect("acquire_buffer call");
            granted.expect("buffer granted");
            client
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
    const REAP_SIGNAL_TIMEOUT: Timeout = Timeout::from_millis(5);

    let client = connect(resolved(importer(), SPARE_VIEW, Path::ROOT.as_str()));
    let data_region = acquire_buffer(&client);
    let data = data_region
        .map_full(UserMemFlags::ReadWrite)
        .expect("map data buffer");

    let names: Vec<String> = (0..FILLER_LIMIT)
        .map(|index| format!("/filler-{index}"))
        .collect();
    create_filler_tree(&client, &data, &names);
    let spare_sessions = buffered_sessions(SPARE_VIEW, RELEASED_SESSIONS);

    let fillers = occupy_mapping_slots(&client, &names);
    kernel_tests::kassert!(fillers.len() < FILLER_LIMIT);

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
    kernel_tests::kassert_eq!(
        client.read(imported.fd, 0, length).expect("read call"),
        Err(FileSystemError::NoSpace)
    );
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

    drop(spare_sessions);
    let turn = Signal::create().expect("signal create");
    let mut read = client.read(imported.fd, 0, length).expect("read call");
    for _ in 0..REAP_ATTEMPTS {
        if read.is_ok() {
            break;
        }
        let _ = turn.wait(syscall::SIGNALED, REAP_SIGNAL_TIMEOUT);
        read = client.read(imported.fd, 0, length).expect("read call");
    }

    let count = read.expect("degraded store maps on demand");
    kernel_tests::kassert_eq!(count, length);
    kernel_tests::kassert_eq!(buffer_head(&data, PAYLOAD.len()), PAYLOAD);
}

#[kernel_test]
fn filesystem_refuses_to_change_the_image() {
    let client = connect(image_session());

    let writable = client
        .open(path(IMAGE_FILE), AccessMode::ReadWrite, OpenFlags::NONE)
        .expect("open call");
    kernel_tests::kassert_eq!(writable, Err(FileSystemError::ReadOnly));

    let truncated = client
        .open(path(IMAGE_FILE), AccessMode::Read, OpenFlags::TRUNCATE)
        .expect("open call");
    kernel_tests::kassert_eq!(truncated, Err(FileSystemError::WrongMode));

    let created = client
        .open(path("/fresh"), AccessMode::Write, OpenFlags::CREATE)
        .expect("open call");
    kernel_tests::kassert_eq!(created, Err(FileSystemError::ReadOnly));

    let removed = client
        .unlink(path(IMAGE_FILE), NodeKind::File)
        .expect("unlink call");
    kernel_tests::kassert_eq!(removed, Err(FileSystemError::ReadOnly));

    let absent = client
        .unlink(path("/absent"), NodeKind::File)
        .expect("unlink call");
    kernel_tests::kassert_eq!(absent, Err(FileSystemError::NotFound));

    let through_a_file = client
        .unlink(path("/testrunner/sample/inner"), NodeKind::File)
        .expect("unlink call");
    kernel_tests::kassert_eq!(through_a_file, Err(FileSystemError::NotADirectory));
}

#[kernel_test]
fn filesystem_holds_a_descriptor_to_its_mode() {
    let client = connect(ram_session());
    let region = acquire_buffer(&client);
    let buffer = region
        .map_full(UserMemFlags::ReadWrite)
        .expect("map data buffer");

    let opened = open(&client, "/write-only", AccessMode::Write, OpenFlags::CREATE);
    let payload = b"mode";
    let count = write_all(&client, &buffer, &opened, 0, payload);
    kernel_tests::kassert_eq!(count, payload.len() as u32);

    let read = client.read(opened.fd, 0, 4).expect("read call");
    kernel_tests::kassert_eq!(read, Err(FileSystemError::WrongMode));

    let stated = client.stat(opened.fd).expect("stat call");
    kernel_tests::kassert_eq!(stated, Err(FileSystemError::WrongMode));

    let window = client
        .backing_memory(opened.fd)
        .expect("backing_memory call");
    kernel_tests::kassert_eq!(window.err(), Some(FileSystemError::WrongMode));

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
    let client = connect(ram_session());
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
    let client = connect(ram_session());

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

    let refused = client.stat(stale.fd).expect("stat call");
    kernel_tests::kassert_eq!(refused.err(), Some(FileSystemError::BadDescriptor));
    let alive = client.stat(fresh.fd).expect("stat call").expect("stat ok");
    kernel_tests::kassert_eq!(alive.kind, NodeKind::File);
}

#[kernel_test]
fn filesystem_isolates_descriptors_between_sessions() {
    let owner = connect(ram_session());
    let stranger = connect(ram_session());

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
fn filesystem_creates_nothing_for_a_call_without_a_reply_channel() {
    let without_reply = connect_without_reply(connect(ram_session()));
    let _ = without_reply.open(
        path(UNANSWERED_PATH),
        AccessMode::ReadWrite,
        OpenFlags::CREATE,
    );

    let client = connect(ram_session());
    let missing = client
        .open(path(UNANSWERED_PATH), AccessMode::Read, OpenFlags::NONE)
        .expect("open call")
        .map(|opened| opened.kind);
    let created = open(
        &client,
        UNANSWERED_PATH,
        AccessMode::ReadWrite,
        OpenFlags::CREATE,
    );

    kernel_tests::kassert_eq!(missing, Err(FileSystemError::NotFound));
    kernel_tests::kassert_eq!(created.kind, NodeKind::File);
}

#[kernel_test]
fn filesystem_serves_on_after_an_unknown_ordinal() {
    let bogus = connect_tampered(
        connect(ram_session()),
        Tamper {
            unknown_ordinal: true,
            ..Tamper::NONE
        },
    );
    let refused = bogus.stat(NodeDescriptor::from_raw(0));
    kernel_tests::kassert_eq!(refused.err(), Some(IpcError::PeerClosed));

    let client = connect(ram_session());
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
    let malformed = connect_tampered(
        connect(ram_session()),
        Tamper {
            empty_body: true,
            ..Tamper::NONE
        },
    );
    let refused = malformed.stat(NodeDescriptor::from_raw(0));
    kernel_tests::kassert_eq!(refused.err(), Some(IpcError::PeerClosed));
}

fn send_unparsed_frame_with_capability<K: kernel_objects::ObjectKind>(
    session: &Port,
    capability: OwnedHandle<K>,
) {
    let mut frame = [0u8; MESSAGE_INLINE_MAX];
    Header::new(u64::MAX)
        .encode(&mut frame)
        .expect("frame header");
    frame[HEADER_SIZE] = TERMINATOR;
    crate::ipc_raw::write_frame(&frame[..=HEADER_SIZE], &[capability.into_raw().raw()]);
    kernel_tests::kassert_eq!(port_send(session.as_raw(), FILE_SYSTEM_REPLY_TIMEOUT), 0);
}

#[kernel_test]
fn filesystem_closes_the_capability_of_an_unparsed_frame() {
    let capability = ram_port();
    let signal = Signal::create().expect("signal to ride the frames");
    for _ in 0..LEAK_FRAMES {
        let rider = signal
            .duplicate(Rights::READ | Rights::TRANSFER, None)
            .expect("capability for the frame");
        send_unparsed_frame_with_capability(&capability, rider);
    }
}

#[kernel_test]
fn filesystem_vends_nothing_for_a_call_without_a_reply_channel() {
    let client = connect(image_session());
    let opened = open(&client, IMAGE_FILE, AccessMode::Read, OpenFlags::NONE);

    let without_reply = connect_without_reply(client);
    for _ in 0..LEAK_FRAMES {
        let _ = without_reply.backing_memory(opened.fd);
    }
    kernel_tests::kassert!(vends_a_buffer());
}

#[kernel_test]
fn filesystem_does_not_pin_a_backing_for_a_call_without_a_reply_channel() {
    const BACKED: &str = "/unanswered-backing";

    let client = connect(ram_session());
    let data_region = acquire_buffer(&client);
    let data = data_region
        .map_full(UserMemFlags::ReadWrite)
        .expect("map data buffer");
    let opened = open(&client, BACKED, AccessMode::ReadWrite, OpenFlags::CREATE);
    write_all(&client, &data, &opened, 0, b"body");

    let second = connect(ram_session());
    let reopened = open(&second, BACKED, AccessMode::ReadWrite, OpenFlags::NONE);
    let without_reply = connect_without_reply(second);
    let _ = without_reply.backing_memory(reopened.fd);

    data.copy_in(b"x").expect("payload into data buffer");
    let grown = client
        .write(opened.fd, USERLAND_PAGE_SIZE, 1)
        .expect("write call")
        .expect("a call without a reply channel does not pin");
    kernel_tests::kassert_eq!(grown.count, 1);

    let backing = MemoryRegion::from(
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
    let client = connect(image_session());
    let Ok(granted) = client.acquire_buffer(BUFFER_SIZE) else {
        return false;
    };
    granted.is_ok()
}

fn take_and_drop_session() {
    let client = connect(ram_session());
    let buffer = client
        .acquire_buffer(REAP_BUFFER_SIZE)
        .expect("acquire_buffer call")
        .expect("buffer granted");
    drop(MemoryRegion::from(buffer));
}

fn run_client_fixture() {
    let client = spawn_entry(CLIENT_FIXTURE, ram_port());
    client
        .join(JOIN_TIMEOUT)
        .expect("client fixture terminates");
    kernel_tests::kassert_eq!(client.exit_code().expect("exit code"), 0);
}

#[kernel_test]
fn filesystem_reaps_the_session_of_a_terminated_client() {
    for _ in 0..REAP_SESSIONS {
        run_client_fixture();
    }
}

#[kernel_test]
fn filesystem_reaps_the_session_of_a_closed_capability() {
    for _ in 0..REAP_SESSIONS {
        take_and_drop_session();
    }
}

#[kernel_test]
fn filesystem_reaps_only_the_session_of_the_gone_badge() {
    let keeper = connect(ram_session());
    let kept = open(&keeper, "/kept", AccessMode::ReadWrite, OpenFlags::CREATE);
    let keeper_buffer = acquire_buffer(&keeper);

    for _ in 0..REAP_SESSIONS {
        take_and_drop_session();
    }

    let mine = keeper.stat(kept.fd).expect("stat call").expect("stat ok");
    kernel_tests::kassert_eq!(mine.kind, NodeKind::File);
    let occupied = keeper
        .acquire_buffer(BUFFER_SIZE)
        .expect("acquire_buffer call");
    kernel_tests::kassert_eq!(occupied.err(), Some(FileSystemError::AlreadyExists));
    drop(keeper_buffer);
}
