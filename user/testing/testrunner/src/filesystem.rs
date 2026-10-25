//! E2E файловой подсистемы на композиции плана testrunner.

use alloc::{format, string::String, vec, vec::Vec};

use filesystem_abi::{
    APPEND_OFFSET, AccessMode, DirCursor, DirEntry, FILE_SYSTEM_OPEN_ROOT_TIMEOUT, FileSystemError,
    NAME_MAX, NodeDescriptor, NodeKind, OpenFlags, Opened, Path, file_system, file_system_mount,
    namespace,
};
use filesystem_client::{Failure, FileSystem};
use ipc::{
    CallError, Channel, Incoming, Outgoing, Unsent,
    wire::{
        FieldCursor, HEADER_SIZE, Header, IpcError, MESSAGE_INLINE_MAX, MessageBuf, TERMINATOR,
    },
};
use kernel_objects::{
    Delivery, Mapping, MemoryRegion, OwnedHandle, Port, Resource, Session, SessionTransport, Signal,
};
use rootkeeper::relay::{ComponentNames, LogSources};
use runtime::{PortTransport, Timeout, UserMemFlags, port_send, spawn_owned};
use spin::Once;
use syscall::Rights;
use userland::USERLAND_PAGE_SIZE;
use userland_image::ExportIndex;

use crate::{
    composition::{BOOT_PLAN_ENTRY, Composition, Importer, Mounted, admin, image, pause, view},
    filesystem_io::IoClient,
    fixture::{JOIN_TIMEOUT, spawn_entry},
    log_relay::SharedLogSessions,
    stress::{answered, expire},
};

pub(crate) const FILE_SYSTEM_REPLY_TIMEOUT: Timeout = Timeout::from_secs(2);

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
const ABOVE_MOUNT_VIEW: &str = "above";
const BELOW_MOUNT_VIEW: &str = "below";

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
    view(ABOVE_MOUNT_VIEW, "/above"),
    view(BELOW_MOUNT_VIEW, "/above/below"),
    admin("below-admin"),
];

pub(crate) const BUFFER_SIZE: u32 = 4096;

const LISTING_ENTRIES: usize = 70;

pub(crate) const COMPONENT_DIRECTORY: &str = "/testrunner";

pub(crate) const IMAGE_FILE: &str = "/testrunner/sample";
pub(crate) const IMAGE_FILE_BODY: &[u8] = b"hello from the userland image\n";

const IMAGE_EMPTY_FILE: &str = "/testrunner/empty";

const UNANSWERED_PATH: &str = "/unanswered";

pub(crate) const LEAK_ROUNDS: usize = syscall::HANDLE_TABLE_CAPACITY as usize + 128;

const REAP_SESSIONS: usize = 40;

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
        rootkeeper::SEARCH_LIMITS,
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
    let resolved = namespace_client(&view).resolve(text).wait();
    if let Err(CallError::Remote(FileSystemError::Unresponsive)) = resolved {
        expire(
            format_args!("requirement={requirement} path={text}"),
            "resolve",
            "open_root",
            FILE_SYSTEM_OPEN_ROOT_TIMEOUT,
        );
    }
    let session = answered(
        resolved,
        format_args!("requirement={requirement} path={text}"),
        "resolve",
        NAMESPACE_REPLY_TIMEOUT,
        "path is covered by a mount",
    );
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
    let window = answered(
        client.open_root(scope).wait(),
        format_args!("path={scope}"),
        "open_root",
        FILE_SYSTEM_REPLY_TIMEOUT,
        "window granted",
    );
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
        .unlink(text, NodeKind::Directory)
        .wait()
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

pub(crate) fn connect(session: Session<file_system::Protocol>) -> IoClient {
    IoClient::new(session.client())
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

fn connect_tampered(client: IoClient, tamper: Tamper) -> file_system::Client<Tampered> {
    file_system::Client::new(Tampered {
        port: client.into_channel(),
        tamper,
    })
}

fn connect_without_reply(client: IoClient) -> file_system::Client<Tampered> {
    connect_tampered(
        client,
        Tamper {
            one_way: true,
            ..Tamper::NONE
        },
    )
}

pub(crate) fn open(
    client: &file_system::Client<SessionTransport>,
    text: &str,
    mode: AccessMode,
    flags: OpenFlags,
) -> Opened {
    client.open(text, mode, flags).wait().expect("open ok")
}

fn close(client: &file_system::Client<SessionTransport>, opened: &Opened) {
    client.close(opened.fd).wait().expect("close ok");
}

pub(crate) fn write_all(client: &IoClient, opened: &Opened, offset: u64, payload: &[u8]) -> u32 {
    let written = client.write(opened.fd, offset, payload).expect("write ok");
    written.count
}

pub(crate) fn mapped_head(buffer: &kernel_objects::MappedPages, len: usize) -> Vec<u8> {
    let mut bytes = vec![0; len];
    buffer.copy_out(&mut bytes).expect("buffer head");
    bytes
}

pub(crate) fn buffer_head(buffer: &[u8], len: usize) -> Vec<u8> {
    buffer[..len].to_vec()
}

fn collect_entries(buffer: &[u8], count: u32, names: &mut Vec<String>) -> usize {
    let mut offset = 0;
    for _ in 0..count {
        let decoded = DirEntry::decode(&buffer[offset..]).expect("directory record");
        names.push(String::from(decoded.entry.name));
        offset += decoded.len;
    }
    offset
}

fn entry_name(index: usize) -> String {
    format!("{index:0>NAME_MAX$}")
}

pub(crate) fn walk_directory(
    client: &IoClient,
    buffer: &mut [u8],
    fd: NodeDescriptor,
) -> Vec<String> {
    let mut names = Vec::new();
    let mut from = DirCursor::START;
    loop {
        let listed = client.list(fd, from, buffer).expect("list ok");
        collect_entries(buffer, listed.count, &mut names);
        if listed.end {
            return names;
        }
        test_harness::check!(listed.next.raw() > from.raw());
        from = listed.next;
    }
}

#[test_harness::test]
fn filesystem_creates_and_writes_a_file() {
    let client = connect(ram_session());

    let opened = open(
        &client,
        "/created",
        AccessMode::ReadWrite,
        OpenFlags::CREATE,
    );
    test_harness::check_eq!(opened.info.kind, NodeKind::File);
    test_harness::check_eq!(opened.info.size, 0);

    let payload = b"hello, file system";
    let count = write_all(&client, &opened, 0, payload);
    test_harness::check_eq!(count, payload.len() as u32);

    let info = client.stat(opened.fd).wait().expect("stat ok");
    test_harness::check_eq!(info.size, payload.len() as u64);

    let again = client
        .open(
            "/created",
            AccessMode::Read,
            OpenFlags::CREATE | OpenFlags::EXCLUSIVE,
        )
        .wait();
    test_harness::check_eq!(
        again,
        Err(CallError::Remote(FileSystemError::AlreadyExists))
    );
}

#[test_harness::test]
fn filesystem_reads_at_offset() {
    let client = connect(ram_session());
    let mut buffer = vec![0; BUFFER_SIZE as usize];

    let opened = open(&client, "/digits", AccessMode::ReadWrite, OpenFlags::CREATE);
    let payload = b"0123456789";
    write_all(&client, &opened, 0, payload);
    buffer[..16].fill(0xA5);

    let count = client
        .read(opened.fd, 4, &mut buffer[..6])
        .expect("read ok");
    test_harness::check_eq!(count, 6);
    test_harness::check_eq!(buffer_head(&buffer, 6), b"456789");

    buffer[..16].fill(0xA5);
    let count = client
        .read(opened.fd, 8, &mut buffer[..12])
        .expect("short read ok");
    test_harness::check_eq!(count, 2);
    let mut expected = vec![0xA5; 16];
    expected[..2].copy_from_slice(b"89");
    test_harness::check_eq!(buffer_head(&buffer, 16), expected);

    buffer[..16].fill(0xA5);
    let tail = client
        .read(opened.fd, payload.len() as u64, &mut buffer[..4])
        .expect("read ok");
    test_harness::check_eq!(tail, 0);
    expected.fill(0xA5);
    test_harness::check_eq!(buffer_head(&buffer, 16), expected);
}

#[test_harness::test]
fn filesystem_stats_file_and_directory() {
    let client = connect(ram_session());

    let directory = open(
        &client,
        "/stat-dir",
        AccessMode::Read,
        OpenFlags::CREATE | OpenFlags::DIRECTORY,
    );
    test_harness::check_eq!(directory.info.kind, NodeKind::Directory);
    test_harness::check_eq!(directory.info.size, 0);

    let file = open(
        &client,
        "/stat-dir/entry",
        AccessMode::ReadWrite,
        OpenFlags::CREATE,
    );
    write_all(&client, &file, 0, b"stat");

    let file_info = client.stat(file.fd).wait().expect("stat ok");
    test_harness::check_eq!(file_info.kind, NodeKind::File);
    test_harness::check_eq!(file_info.size, 4);

    let directory_info = client.stat(directory.fd).wait().expect("stat ok");
    test_harness::check_eq!(directory_info.kind, NodeKind::Directory);
    test_harness::check_eq!(directory_info.size, 1);
}

#[test_harness::test]
fn filesystem_stat_of_an_open_node_matches_its_opening() {
    let client = connect(ram_session());
    let opened = open(
        &client,
        "/identity-stat",
        AccessMode::ReadWrite,
        OpenFlags::CREATE,
    );

    let info = client.stat(opened.fd).wait().expect("stat ok");

    test_harness::check_eq!(info, opened.info);
    test_harness::check!(info.volume != 0);
}

#[test_harness::test]
fn filesystem_volumes_of_different_servers_differ() {
    let [ram, image] = [ram_session(), image_session()].map(|session| {
        open(
            &connect(session),
            Path::ROOT.as_str(),
            AccessMode::Read,
            OpenFlags::DIRECTORY,
        )
    });

    test_harness::check!(ram.info.volume != image.info.volume);
}

#[test_harness::test]
fn filesystem_lists_directory_across_batches() {
    let client = connect(ram_session());
    let mut buffer = vec![0; BUFFER_SIZE as usize];

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
        buffer.fill(0xA5);
        let listed = client
            .list(directory.fd, from, &mut buffer)
            .expect("list ok");
        batches += 1;
        let bytes = &buffer;
        let used = collect_entries(bytes, listed.count, &mut names);
        test_harness::check!(bytes[used..].iter().all(|byte| *byte == 0xA5));
        if listed.end {
            break;
        }
        test_harness::check!(listed.count > 0 && listed.next.raw() > from.raw());
        from = listed.next;
    }

    test_harness::check!(batches >= 2);
    names.sort_unstable();
    names.dedup();
    test_harness::check_eq!(names.len(), LISTING_ENTRIES);
}

#[test_harness::test]
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
        .unlink("/removed", NodeKind::File)
        .wait()
        .expect("unlink ok");

    let reopened = client
        .open("/removed", AccessMode::Read, OpenFlags::NONE)
        .wait();
    test_harness::check_eq!(reopened, Err(CallError::Remote(FileSystemError::NotFound)));
    let again = client.unlink("/removed", NodeKind::File).wait();
    test_harness::check_eq!(again, Err(CallError::Remote(FileSystemError::NotFound)));
}

#[test_harness::test]
fn filesystem_rejects_write_through_a_read_only_descriptor() {
    let client = connect(ram_session());

    let opened = open(&client, "/read-only", AccessMode::Read, OpenFlags::CREATE);
    let payload = b"deny";
    let written = client.write(opened.fd, 0, payload);
    test_harness::check_eq!(written, Err(CallError::Remote(FileSystemError::WrongMode)));

    let resized = client.set_size(opened.fd, 16).wait();
    test_harness::check_eq!(resized, Err(CallError::Remote(FileSystemError::WrongMode)));

    let truncated = client
        .open("/read-only", AccessMode::Read, OpenFlags::TRUNCATE)
        .wait();
    test_harness::check_eq!(
        truncated,
        Err(CallError::Remote(FileSystemError::WrongMode))
    );
}

#[test_harness::test]
fn a_view_does_not_resolve_outside_its_scope() {
    let uncovered = view_of(UNCOVERED_VIEW);
    let outside = namespace::Client::new(PortTransport::new(&uncovered))
        .with_wait(OUT_OF_SCOPE_RESOLVE_TIMEOUT)
        .resolve(Path::ROOT.as_str())
        .wait();
    test_harness::check_eq!(outside.err(), Some(CallError::Ipc(IpcError::Timeout)));
}

#[test_harness::test]
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
        .resolve_pair("/a/x", "/a/y")
        .wait()
        .expect("the mount of the scope covers the pair");
    test_harness::check_eq!(pair.root_len, 2);
}

#[test_harness::test]
fn a_session_mounts_once() {
    let mounted = Mounted::new(importer(), "other", ram_session());
    let again = mounted.mount(ram_session());
    test_harness::check_eq!(
        again,
        Err(CallError::Remote(FileSystemError::AlreadyExists))
    );
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
    .info
    .kind
}

#[test_harness::test]
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

    test_harness::check!(first.ended(MOUNT_END_TIMEOUT));
    test_harness::check_eq!(replaced_entry_kind(), NodeKind::File);
    drop(first);
    test_harness::check_eq!(replaced_entry_kind(), NodeKind::File);
}

#[test_harness::test]
fn namespace_resolves_each_path_to_the_longest_prefix() {
    let capability = view_of(ROOT_VIEW);
    let namespace = namespace_client(&capability);

    let image = namespace
        .resolve(IMAGE_MOUNT)
        .wait()
        .expect("image mount covers the path");
    drop(image);

    connect(ram_session())
        .open(
            "/inner",
            AccessMode::ReadWrite,
            OpenFlags::CREATE | OpenFlags::DIRECTORY,
        )
        .wait()
        .expect("directory created");

    let ram = namespace
        .resolve("/tmp/inner")
        .wait()
        .expect("ram mount covers the path");
    drop(ram);
}

#[test_harness::test]
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
    test_harness::check_eq!(opened.info.kind, NodeKind::File);

    let absent = connect(image_session())
        .open(GRAFT_ENTRY, AccessMode::Read, OpenFlags::NONE)
        .wait();
    test_harness::check_eq!(absent, Err(CallError::Remote(FileSystemError::NotFound)));
}

#[test_harness::test]
fn namespace_rejects_admin_operations_on_the_service_port() {
    let capability = view_of(ROOT_VIEW);
    let refused = file_system_mount::Client::new(PortTransport::new(&capability))
        .with_wait(FILE_SYSTEM_REPLY_TIMEOUT)
        .mount("/invalid", Session::from_handle(ram_port()))
        .wait();
    test_harness::check_eq!(refused.err(), Some(CallError::Ipc(IpcError::PeerClosed)));

    let mounted = namespace_client(&view_of(ROOT_VIEW))
        .resolve(IMAGE_MOUNT)
        .wait()
        .expect("image mount survives the rejected frame");
    drop(mounted);
}

#[test_harness::test]
fn namespace_reports_an_unresponsive_mount() {
    let resolved = namespace_client(&view_of(STUCK_VIEW))
        .resolve(Path::ROOT.as_str())
        .wait();
    test_harness::check_eq!(
        resolved.err(),
        Some(CallError::Remote(FileSystemError::Unresponsive))
    );
}

#[test_harness::test]
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
        .resolve_pair("/tmp/pair-left/first", "/tmp/pair-right/second")
        .wait()
        .expect("one mount covers the pair");
    test_harness::check_eq!(pair.root_len, RAM_MOUNT.len() as u32);
    let opened = open(
        &connect(Session::from_session(pair.session)),
        "/pair-left/first",
        AccessMode::Read,
        OpenFlags::NONE,
    );
    test_harness::check_eq!(opened.info.kind, NodeKind::File);
}

#[test_harness::test]
fn namespace_keeps_the_pair_scope_inside_the_mount_root() {
    let pair = namespace_client(&view_of(ROOT_VIEW))
        .resolve_pair(RAM_MOUNT, "/tmp/inside")
        .wait()
        .expect("one mount covers the pair");
    test_harness::check_eq!(pair.root_len, RAM_MOUNT.len() as u32);
}

#[test_harness::test]
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
        .resolve_pair("/tmp/first", "/paired-second/second")
        .wait()
        .map(|pair| pair.root_len);
    test_harness::check_eq!(across, Err(CallError::Remote(FileSystemError::CrossMount)));

    drop(
        namespace
            .resolve("/paired-second/second")
            .wait()
            .expect("second mount covers the path"),
    );
}

#[test_harness::test]
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
    for _ in 0..LEAK_ROUNDS {
        let _ = without_reply
            .resolve_pair("/tmp/first", "/tmp/second")
            .wait();
    }

    let covered = namespace_client(&view_of(ROOT_VIEW))
        .resolve(RAM_MOUNT)
        .wait();
    test_harness::check!(covered.is_ok());
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

#[test_harness::test]
fn namespace_pair_scope_survives_a_table_change() {
    let mounted = Mounted::new(importer(), "paired-detached", ram_session());
    let pair = namespace_client(&view_of(ROOT_VIEW))
        .resolve_pair("/paired-detached/first", "/paired-detached/second")
        .wait()
        .expect("fixture covers the pair");
    let scope = Session::from_session(pair.session);

    drop(mounted);

    let client = connect(scope);
    close(
        &client,
        &open(&client, "/first", AccessMode::ReadWrite, OpenFlags::CREATE),
    );
    client
        .rename("/first", "/second")
        .wait()
        .expect("rename inside the issued scope");
}

const ROUTER_VIEW_WAITING_REQUESTS_MAX: usize = 8;
const QUEUED_RESOLVE_TIMEOUT: Timeout = Timeout::from_secs(20);
const EVICTION_POLL_INTERVAL: Timeout = Timeout::from_millis(10);
const EVICTION_POLL_ATTEMPTS: usize = 500;
const QUEUE_CALLER_ANSWERED: u32 = 0;
const QUEUE_CALLER_EVICTED: u32 = 1;
const QUEUE_CALLER_FAILED: u32 = 2;

#[test_harness::test]
fn a_full_view_queue_evicts_one_request_and_keeps_the_rest() {
    let capability = view_of(QUEUE_VIEW);
    let raw = capability.as_raw();
    let mut callers: Vec<_> = (0..=ROUTER_VIEW_WAITING_REQUESTS_MAX)
        .map(|_| {
            spawn_owned(move || {
                let client = namespace::Client::new(PortTransport::new(raw))
                    .with_wait(QUEUED_RESOLVE_TIMEOUT);
                match client.resolve(Path::ROOT.as_str()).wait() {
                    Ok(_) => QUEUE_CALLER_ANSWERED,
                    Err(CallError::Remote(FileSystemError::NoSpace)) => QUEUE_CALLER_EVICTED,
                    _ => QUEUE_CALLER_FAILED,
                }
            })
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
    test_harness::check_eq!(evicted.exit_code(), Ok(QUEUE_CALLER_EVICTED));
    test_harness::check!(evicted.join(Timeout::POLL).is_ok());

    let mounted = Mounted::new(importer(), QUEUE_VIEW, ram_session());
    for caller in callers {
        test_harness::check!(caller.wait(JOIN_TIMEOUT).is_ok());
        test_harness::check_eq!(caller.exit_code(), Ok(QUEUE_CALLER_ANSWERED));
        test_harness::check!(caller.join(Timeout::POLL).is_ok());
    }
    drop(mounted);
}

const UNMOUNTED_PATH: &str = "/unmounted/file";
const IMAGE_COMPONENT_ENTRY: &str = "testrunner";
const IMAGE_FILE_IN_MOUNT: &str = "/boot/testrunner/sample";
const WAITING_CALLER_PAUSE: Timeout = Timeout::from_millis(100);
const WAITING_CALLER_NOT_FOUND: u32 = 0;
const WAITING_CALLER_FAILED: u32 = 1;

#[test_harness::test]
fn the_facade_reaches_a_mount_point_without_a_mount_above_it() {
    let mut files = FileSystem::new(Session::from_handle(view_of(ROOT_VIEW)))
        .with_wait(FILE_SYSTEM_REPLY_TIMEOUT);
    let mount = Path::new(IMAGE_MOUNT).expect("mount path is valid");

    let info = files.metadata(mount).expect("mount point metadata");
    test_harness::check_eq!(info.kind, NodeKind::Directory);

    let mut listing = files.read_directory(mount).expect("mount point listing");
    let mut names = Vec::new();
    while let Some(entry) = listing.next_entry().expect("listing entry") {
        names.push(String::from(entry.name));
    }
    test_harness::check!(names.iter().any(|name| name == IMAGE_COMPONENT_ENTRY));

    let mut files = files.with_wait(OUT_OF_SCOPE_RESOLVE_TIMEOUT);
    let outside = files
        .metadata(Path::new(UNMOUNTED_PATH).expect("unmounted path is valid"))
        .err();
    test_harness::check_eq!(outside, Some(Failure::Transport(IpcError::Timeout)));
}

#[test_harness::test]
fn a_pair_with_a_side_above_a_mount_is_not_found() {
    let pair = namespace_client(&view_of(ROOT_VIEW))
        .resolve_pair(Path::ROOT.as_str(), IMAGE_FILE_IN_MOUNT)
        .wait();
    test_harness::check_eq!(
        pair.err(),
        Some(CallError::Remote(FileSystemError::NotFound))
    );
}

#[test_harness::test]
fn a_waiting_request_fails_once_a_mount_appears_below_its_path() {
    let capability = view_of(ABOVE_MOUNT_VIEW);
    let raw = capability.as_raw();
    let caller = spawn_owned(move || {
        let client =
            namespace::Client::new(PortTransport::new(raw)).with_wait(NAMESPACE_REPLY_TIMEOUT);
        match client.resolve(Path::ROOT.as_str()).wait() {
            Err(CallError::Remote(FileSystemError::NotFound)) => WAITING_CALLER_NOT_FOUND,
            _ => WAITING_CALLER_FAILED,
        }
    })
    .expect("waiting caller starts");
    test_harness::check!(caller.wait(WAITING_CALLER_PAUSE).is_err());

    let mounted = Mounted::new(importer(), BELOW_MOUNT_VIEW, ram_session());
    test_harness::check!(caller.wait(JOIN_TIMEOUT).is_ok());
    test_harness::check_eq!(caller.exit_code(), Ok(WAITING_CALLER_NOT_FOUND));
    test_harness::check!(caller.join(Timeout::POLL).is_ok());
    drop(mounted);
}

#[test_harness::test]
fn a_mount_with_an_empty_path_is_refused() {
    let port = importer().port::<file_system_mount::Protocol>(EMPTY_SCOPE_ADMIN);
    let answer = file_system_mount::Client::new(PortTransport::new(&port))
        .with_wait(FILE_SYSTEM_REPLY_TIMEOUT)
        .mount("", ram_session())
        .wait();
    test_harness::check_eq!(answer, Err(CallError::Remote(FileSystemError::InvalidPath)));
}

#[test_harness::test]
fn a_closed_admin_session_ends_its_mount_and_keeps_issued_windows() {
    let mut mounted = Mounted::new(importer(), SHADOW_VIEW, ram_session());
    let window = resolved(importer(), SHADOW_VIEW, Path::ROOT.as_str());
    mounted.close_admin();
    test_harness::check!(mounted.cancelled(MOUNT_END_TIMEOUT));
    drop(mounted);

    let opened = connect(window)
        .open(Path::ROOT.as_str(), AccessMode::Read, OpenFlags::NONE)
        .wait()
        .expect("issued window keeps its root");
    test_harness::check_eq!(opened.info.kind, NodeKind::Directory);
    let rerouted = session_for(SHADOW_MOUNT);
    let image_file = open(
        &connect(rerouted),
        "/sample",
        AccessMode::Read,
        OpenFlags::NONE,
    );
    test_harness::check_eq!(image_file.info.kind, NodeKind::File);
}

#[test_harness::test]
fn filesystem_reads_a_file_from_the_image() {
    let client = connect(image_session());
    let mut buffer = vec![0; BUFFER_SIZE as usize];

    let opened = open(&client, IMAGE_FILE, AccessMode::Read, OpenFlags::NONE);
    test_harness::check_eq!(opened.info.kind, NodeKind::File);
    test_harness::check_eq!(opened.info.size, IMAGE_FILE_BODY.len() as u64);

    let count = client.read(opened.fd, 0, &mut buffer).expect("read ok");
    test_harness::check_eq!(count, IMAGE_FILE_BODY.len() as u32);
    test_harness::check_eq!(buffer_head(&buffer, count as usize), IMAGE_FILE_BODY);
}

#[test_harness::test]
fn filesystem_lists_the_image_directory() {
    let client = connect(image_session());
    let mut buffer = vec![0; BUFFER_SIZE as usize];

    let directory = open(
        &client,
        Path::ROOT.as_str(),
        AccessMode::Read,
        OpenFlags::DIRECTORY,
    );
    test_harness::check_eq!(directory.info.kind, NodeKind::Directory);

    let names = walk_directory(&client, &mut buffer, directory.fd);

    for expected in ["testrunner", "mounts"] {
        test_harness::check!(names.iter().any(|name| name.as_str() == expected));
    }

    let own = open(
        &client,
        COMPONENT_DIRECTORY,
        AccessMode::Read,
        OpenFlags::DIRECTORY,
    );
    let names = walk_directory(&client, &mut buffer, own.fd);
    for expected in ["exec", "sample", "empty"] {
        test_harness::check!(names.iter().any(|name| name.as_str() == expected));
    }
}

#[test_harness::test]
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
        .open("/only-in-ram", AccessMode::Read, OpenFlags::NONE)
        .wait();
    test_harness::check_eq!(absent, Err(CallError::Remote(FileSystemError::NotFound)));
    let foreign = ram
        .open(IMAGE_FILE, AccessMode::Read, OpenFlags::NONE)
        .wait();
    test_harness::check_eq!(foreign, Err(CallError::Remote(FileSystemError::NotFound)));
}

#[test_harness::test]
fn filesystem_hands_out_the_backing_window_of_an_image_file() {
    let client = connect(image_session());
    let mut buffer = vec![0; BUFFER_SIZE as usize];

    let opened = open(&client, IMAGE_FILE, AccessMode::Read, OpenFlags::NONE);
    let count = client.read(opened.fd, 0, &mut buffer).expect("read ok");

    let window = MemoryRegion::from(
        client
            .backing_memory(opened.fd)
            .wait()
            .expect("window granted"),
    );
    let mapped = Mapping::full(&window, UserMemFlags::ReadOnly).expect("map backing window");
    test_harness::check!(mapped.size_bytes() >= opened.info.size);
    test_harness::check_eq!(
        mapped_head(&mapped, opened.info.size as usize),
        buffer_head(&buffer, count as usize)
    );

    test_harness::check!(Mapping::full(&window, UserMemFlags::ReadWrite).is_err());
}

#[test_harness::test]
fn filesystem_serves_an_empty_file_from_the_image() {
    let client = connect(image_session());
    let mut buffer = vec![0; BUFFER_SIZE as usize];

    let opened = open(&client, IMAGE_EMPTY_FILE, AccessMode::Read, OpenFlags::NONE);
    test_harness::check_eq!(opened.info.kind, NodeKind::File);
    test_harness::check_eq!(opened.info.size, 0);

    let info = client.stat(opened.fd).wait().expect("stat ok");
    test_harness::check_eq!(info.kind, NodeKind::File);
    test_harness::check_eq!(info.size, 0);

    let count = client.read(opened.fd, 0, &mut buffer).expect("read ok");
    test_harness::check_eq!(count, 0);

    let refused = client.backing_memory(opened.fd).wait();
    test_harness::check_eq!(
        refused.err(),
        Some(CallError::Remote(FileSystemError::NotSupported))
    );
}

#[test_harness::test]
fn filesystem_ramfs_zeroes_the_gap_of_a_sparse_write() {
    let client = connect(ram_session());
    let mut data = vec![0; BUFFER_SIZE as usize];
    let opened = open(&client, "/sparse", AccessMode::ReadWrite, OpenFlags::CREATE);
    write_all(&client, &opened, 0, b"head");

    let backing = MemoryRegion::from(
        client
            .backing_memory(opened.fd)
            .wait()
            .expect("backing granted"),
    );
    let mapped = Mapping::full(backing, UserMemFlags::ReadWrite).expect("map ramfs backing");
    mapped.copy_in_at(4, b"stale").expect("stale bytes");

    let written = client
        .write(opened.fd, 16, b"tail")
        .expect("sparse write within capacity");
    test_harness::check_eq!(written.count, 4);

    let count = client.read(opened.fd, 0, &mut data[..20]).expect("read ok");
    test_harness::check_eq!(count, 20);
    test_harness::check_eq!(buffer_head(&data, 20), b"head\0\0\0\0\0\0\0\0\0\0\0\0tail");
}

#[test_harness::test]
fn filesystem_ramfs_accepts_a_client_region_without_copying() {
    let creator = connect(ram_session());
    let resource = Resource::self_resource().expect("self resource");
    let region = resource
        .create_virtual(2 * USERLAND_PAGE_SIZE, runtime::MemoryAccess::RW)
        .expect("create shared region");
    let mapped = Mapping::full(region, UserMemFlags::ReadWrite).expect("map shared region");
    mapped.copy_in(b"client").expect("initialize region");
    let handed = mapped
        .region()
        .duplicate(
            Rights::READ | Rights::WRITE | Rights::DUPLICATE | Rights::TRANSFER,
            None,
        )
        .expect("region copy for the server");
    creator
        .create_backed("/client-region", handed, 6)
        .wait()
        .expect("create backed file");

    let reader = connect(ram_session());
    let mut data = vec![0; BUFFER_SIZE as usize];
    let opened = open(
        &reader,
        "/client-region",
        AccessMode::ReadWrite,
        OpenFlags::NONE,
    );
    let count = reader.read(opened.fd, 0, &mut data[..6]).expect("read ok");
    test_harness::check_eq!(count, 6);
    test_harness::check_eq!(buffer_head(&data, 6), b"client");

    mapped.copy_in(b"mapped").expect("change client mapping");
    let count = reader.read(opened.fd, 0, &mut data[..6]).expect("read ok");
    test_harness::check_eq!(count, 6);
    test_harness::check_eq!(buffer_head(&data, 6), b"mapped");

    write_all(&reader, &opened, 0, b"server");
    test_harness::check_eq!(mapped_head(&mapped, 6), b"server");

    let shared = MemoryRegion::from(
        reader
            .backing_memory(opened.fd)
            .wait()
            .expect("backing granted"),
    );
    test_harness::check!(
        shared
            .duplicate(Rights::READ | Rights::TRANSFER, None)
            .is_err()
    );
}

fn region_carrying(payload: &[u8]) -> MemoryRegion {
    let resource = Resource::self_resource().expect("self resource");
    let region = resource
        .create_virtual(USERLAND_PAGE_SIZE, runtime::MemoryAccess::RW)
        .expect("create imported region");
    let mapped = Mapping::full(&region, UserMemFlags::ReadWrite).expect("map imported region");
    mapped.copy_in(payload).expect("payload into the region");
    drop(mapped);
    region
}

#[test_harness::test]
fn filesystem_serves_more_file_regions_than_server_mapping_slots() {
    const FILES: usize = 256;
    const IMPORTED: &str = "/imported";
    const PAYLOAD: &[u8] = b"region bytes";
    let client = connect(resolved(importer(), SPARE_VIEW, Path::ROOT.as_str()));
    let names: Vec<String> = (0..FILES).map(|index| format!("/filler-{index}")).collect();
    for name in &names {
        let opened = open(&client, name, AccessMode::ReadWrite, OpenFlags::CREATE);
        test_harness::check_eq!(
            client.write(opened.fd, 0, b"f").expect("write file").count,
            1
        );
        close(&client, &opened);
    }
    client
        .create_backed(IMPORTED, region_carrying(PAYLOAD), PAYLOAD.len() as u64)
        .wait()
        .expect("import region");

    let opened = open(&client, IMPORTED, AccessMode::ReadWrite, OpenFlags::NONE);
    let mut bytes = [0; PAYLOAD.len()];
    test_harness::check_eq!(
        client
            .read(opened.fd, 0, &mut bytes)
            .expect("read imported file"),
        PAYLOAD.len() as u32
    );
    test_harness::check_eq!(&bytes, PAYLOAD);
    close(&client, &opened);
    for name in names.iter().map(String::as_str).chain([IMPORTED]) {
        client
            .unlink(name, NodeKind::File)
            .wait()
            .expect("remove test file");
    }
}

#[test_harness::test]
fn filesystem_refuses_to_change_the_image() {
    let client = connect(image_session());

    let writable = client
        .open(IMAGE_FILE, AccessMode::ReadWrite, OpenFlags::NONE)
        .wait();
    test_harness::check_eq!(writable, Err(CallError::Remote(FileSystemError::ReadOnly)));

    let truncated = client
        .open(IMAGE_FILE, AccessMode::Read, OpenFlags::TRUNCATE)
        .wait();
    test_harness::check_eq!(
        truncated,
        Err(CallError::Remote(FileSystemError::WrongMode))
    );

    let created = client
        .open("/fresh", AccessMode::Write, OpenFlags::CREATE)
        .wait();
    test_harness::check_eq!(created, Err(CallError::Remote(FileSystemError::ReadOnly)));

    let removed = client.unlink(IMAGE_FILE, NodeKind::File).wait();
    test_harness::check_eq!(removed, Err(CallError::Remote(FileSystemError::ReadOnly)));

    let absent = client.unlink("/absent", NodeKind::File).wait();
    test_harness::check_eq!(absent, Err(CallError::Remote(FileSystemError::NotFound)));

    let through_a_file = client
        .unlink("/testrunner/sample/inner", NodeKind::File)
        .wait();

    test_harness::check_eq!(
        through_a_file,
        Err(CallError::Remote(FileSystemError::NotADirectory))
    );
}

#[test_harness::test]
fn filesystem_holds_a_descriptor_to_its_mode() {
    let client = connect(ram_session());
    let mut buffer = vec![0; BUFFER_SIZE as usize];

    let opened = open(&client, "/write-only", AccessMode::Write, OpenFlags::CREATE);
    let payload = b"mode";
    let count = write_all(&client, &opened, 0, payload);
    test_harness::check_eq!(count, payload.len() as u32);

    let read = client.read(opened.fd, 0, &mut buffer[..4]);
    test_harness::check_eq!(read, Err(CallError::Remote(FileSystemError::WrongMode)));

    let stated = client.stat(opened.fd).wait();
    test_harness::check_eq!(stated, Err(CallError::Remote(FileSystemError::WrongMode)));

    let window = client.backing_memory(opened.fd).wait();
    test_harness::check_eq!(
        window.err(),
        Some(CallError::Remote(FileSystemError::WrongMode))
    );

    let directory = open(
        &client,
        "/write-only-dir",
        AccessMode::Write,
        OpenFlags::CREATE | OpenFlags::DIRECTORY,
    );
    let listed = client.list(directory.fd, DirCursor::START, &mut buffer);
    test_harness::check_eq!(listed, Err(CallError::Remote(FileSystemError::WrongMode)));
}

#[test_harness::test]
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

    let refused = client.stat(stale.fd).wait();
    test_harness::check_eq!(
        refused.err(),
        Some(CallError::Remote(FileSystemError::BadDescriptor))
    );
    let alive = client.stat(fresh.fd).wait().expect("stat ok");
    test_harness::check_eq!(alive.kind, NodeKind::File);
}

#[test_harness::test]
fn filesystem_isolates_descriptors_between_sessions() {
    let owner = connect(ram_session());
    let stranger = connect(ram_session());

    let opened = open(
        &owner,
        "/isolated",
        AccessMode::ReadWrite,
        OpenFlags::CREATE,
    );
    let mine = owner.stat(opened.fd).wait().expect("stat ok");
    test_harness::check_eq!(mine.kind, NodeKind::File);

    let foreign = stranger.stat(opened.fd).wait();
    test_harness::check_eq!(
        foreign,
        Err(CallError::Remote(FileSystemError::BadDescriptor))
    );
}

#[test_harness::test]
fn filesystem_creates_nothing_for_a_call_without_a_reply_channel() {
    let without_reply = connect_without_reply(connect(ram_session()));
    let _ = without_reply
        .open(UNANSWERED_PATH, AccessMode::ReadWrite, OpenFlags::CREATE)
        .wait();

    let client = connect(ram_session());
    let missing = client
        .open(UNANSWERED_PATH, AccessMode::Read, OpenFlags::NONE)
        .wait()
        .map(|opened| opened.info.kind);
    let created = open(
        &client,
        UNANSWERED_PATH,
        AccessMode::ReadWrite,
        OpenFlags::CREATE,
    );

    test_harness::check_eq!(missing, Err(CallError::Remote(FileSystemError::NotFound)));
    test_harness::check_eq!(created.info.kind, NodeKind::File);
}

#[test_harness::test]
fn filesystem_serves_on_after_an_unknown_ordinal() {
    let bogus = connect_tampered(
        connect(ram_session()),
        Tamper {
            unknown_ordinal: true,
            ..Tamper::NONE
        },
    );
    let refused = bogus.stat(NodeDescriptor::from_raw(0)).wait();
    test_harness::check_eq!(refused.err(), Some(CallError::Ipc(IpcError::PeerClosed)));

    let client = connect(ram_session());
    let opened = open(
        &client,
        "/after-unknown-ordinal",
        AccessMode::ReadWrite,
        OpenFlags::CREATE,
    );
    test_harness::check_eq!(opened.info.kind, NodeKind::File);
}

#[test_harness::test]
fn filesystem_releases_the_reply_of_an_unparsed_frame() {
    let malformed = connect_tampered(
        connect(ram_session()),
        Tamper {
            empty_body: true,
            ..Tamper::NONE
        },
    );
    let refused = malformed.stat(NodeDescriptor::from_raw(0)).wait();
    test_harness::check_eq!(refused.err(), Some(CallError::Ipc(IpcError::PeerClosed)));
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
    test_harness::check_eq!(port_send(session.as_raw(), FILE_SYSTEM_REPLY_TIMEOUT), 0);
}

#[test_harness::test]
fn filesystem_closes_the_capability_of_an_unparsed_frame() {
    let capability = ram_port();
    let signal = Signal::create().expect("signal to ride the frames");
    for _ in 0..LEAK_ROUNDS {
        let rider = signal
            .duplicate(Rights::READ | Rights::TRANSFER, None)
            .expect("capability for the frame");
        send_unparsed_frame_with_capability(&capability, rider);
    }
}

#[test_harness::test]
fn filesystem_vends_nothing_for_a_call_without_a_reply_channel() {
    let client = connect(image_session());
    let opened = open(&client, IMAGE_FILE, AccessMode::Read, OpenFlags::NONE);

    let without_reply = connect_without_reply(client);
    for _ in 0..LEAK_ROUNDS {
        let _ = without_reply.backing_memory(opened.fd).wait();
    }
    test_harness::check!(vends_an_exchange());
}

#[test_harness::test]
fn filesystem_does_not_pin_a_backing_for_a_call_without_a_reply_channel() {
    const BACKED: &str = "/unanswered-backing";

    let client = connect(ram_session());
    let opened = open(&client, BACKED, AccessMode::ReadWrite, OpenFlags::CREATE);
    write_all(&client, &opened, 0, b"body");

    let second = connect(ram_session());
    let reopened = open(&second, BACKED, AccessMode::ReadWrite, OpenFlags::NONE);
    let without_reply = connect_without_reply(second);
    let _ = without_reply.backing_memory(reopened.fd).wait();

    let grown = client
        .write(opened.fd, USERLAND_PAGE_SIZE, b"x")
        .expect("a call without a reply channel does not pin");
    test_harness::check_eq!(grown.count, 1);

    let backing = MemoryRegion::from(
        client
            .backing_memory(opened.fd)
            .wait()
            .expect("backing granted"),
    );
    let capacity = backing.inspect().expect("inspect backing").size_bytes;
    let pinned = client.write(opened.fd, capacity, b"y");
    test_harness::check_eq!(pinned, Err(CallError::Remote(FileSystemError::NoSpace)));

    let growing = open(
        &client,
        "/heap-backed",
        AccessMode::ReadWrite,
        OpenFlags::CREATE,
    );
    let written = client
        .write(growing.fd, capacity, b"x")
        .expect("a file without a delegated backing grows");
    test_harness::check_eq!(written.count, 1);
}

fn vends_an_exchange() -> bool {
    let client = connect(image_session());
    client.start_io().wait().is_ok()
}

fn take_and_drop_session() {
    let client = connect(ram_session());
    drop(client.start_io().wait().expect("exchange granted"));
}

fn run_client_fixture() {
    let client = spawn_entry(CLIENT_FIXTURE, ram_port());
    client
        .join(JOIN_TIMEOUT)
        .expect("client fixture terminates");
    test_harness::check_eq!(client.exit_code().expect("exit code"), 0);
}

#[test_harness::test]
fn filesystem_reaps_the_session_of_a_terminated_client() {
    for _ in 0..REAP_SESSIONS {
        run_client_fixture();
    }
}

#[test_harness::test]
fn filesystem_reaps_only_the_session_of_the_gone_badge() {
    let keeper = connect(ram_session());
    let kept = open(&keeper, "/kept", AccessMode::ReadWrite, OpenFlags::CREATE);
    let keeper_exchange = keeper.start_io().wait().expect("exchange granted");

    for _ in 0..REAP_SESSIONS {
        take_and_drop_session();
    }

    let mine = keeper.stat(kept.fd).wait().expect("stat ok");
    test_harness::check_eq!(mine.kind, NodeKind::File);
    let occupied = keeper.start_io().wait();
    test_harness::check_eq!(
        occupied.err(),
        Some(CallError::Remote(FileSystemError::AlreadyExists))
    );
    drop(keeper_exchange);
}

#[test_harness::test]
fn filesystem_windows_alias_the_file_for_read_and_write() {
    use filesystem_abi::IoPhase;
    use filesystem_client::SharedRegion as _;
    let client = connect(ram_session());
    let opened = open(
        &client,
        "/alias-window",
        AccessMode::ReadWrite,
        OpenFlags::CREATE,
    );
    client
        .set_size(opened.fd, 4)
        .wait()
        .expect("initial length");
    let backing = MemoryRegion::from(
        client
            .backing_memory(opened.fd)
            .wait()
            .expect("observer capability"),
    );
    let observer = Mapping::full(backing, UserMemFlags::ReadWrite).expect("observer mapping");
    let mut before_commit = [0; 4];
    client
        .with_window(IoPhase::Write, opened.fd, 0, 4, |region, start, count| {
            region.write_at(start, b"file")?;
            observer
                .copy_out(&mut before_commit)
                .expect("observe before commit");
            Ok(count as u32)
        })
        .expect("commit direct write");
    test_harness::check_eq!(&before_commit, b"file");
    let mut from_window = [0; 4];
    client
        .with_window(IoPhase::Read, opened.fd, 0, 4, |region, start, count| {
            observer
                .copy_in(b"live")
                .expect("change after read preparation");
            region.read_at(start, &mut from_window)?;
            Ok(count as u32)
        })
        .expect("finish direct read");
    test_harness::check_eq!(&from_window, b"live");
    close(&client, &opened);
}

#[test_harness::test]
fn filesystem_write_only_window_does_not_disclose_file_bytes() {
    use filesystem_abi::IoPhase;
    use filesystem_client::SharedRegion as _;
    let client = connect(ram_session());
    let readable = open(
        &client,
        "/write-only-window",
        AccessMode::ReadWrite,
        OpenFlags::CREATE,
    );
    client
        .write(readable.fd, 0, b"private!")
        .expect("initial bytes");
    let backing = MemoryRegion::from(
        client
            .backing_memory(readable.fd)
            .wait()
            .expect("observer capability"),
    );
    let observer = Mapping::full(backing, UserMemFlags::ReadOnly).expect("observer mapping");
    let writer = open(
        &client,
        "/write-only-window",
        AccessMode::Write,
        OpenFlags::NONE,
    );
    let mut fresh = [0xA5; 8];
    let mut pending = [0; 8];
    client
        .with_window(IoPhase::Write, writer.fd, 0, 8, |region, start, count| {
            region.read_at(start, &mut fresh)?;
            region.write_at(start, b"public!!")?;
            observer
                .copy_out(&mut pending)
                .expect("observe pending write");
            Ok(count as u32)
        })
        .expect("commit write-only window");
    let mut committed = [0; 8];
    observer
        .copy_out(&mut committed)
        .expect("observe committed write");
    test_harness::check_eq!(fresh, [0; 8]);
    test_harness::check_eq!(&pending, b"private!");
    test_harness::check_eq!(&committed, b"public!!");
    close(&client, &writer);
    close(&client, &readable);
}

#[test_harness::test]
fn filesystem_rejects_an_oversized_finish_and_keeps_serving() {
    use filesystem_abi::IoPhase;
    let client = connect(ram_session());
    let opened = open(
        &client,
        "/invalid-finish",
        AccessMode::ReadWrite,
        OpenFlags::CREATE,
    );
    client
        .set_size(opened.fd, 4)
        .wait()
        .expect("initial length");
    let refused = client.with_window(IoPhase::Write, opened.fd, 0, 4, |_, _, count| {
        Ok(count as u32 + 1)
    });
    test_harness::check_eq!(
        refused.err(),
        Some(CallError::Remote(FileSystemError::BufferTooSmall))
    );
    let mut bytes = [0xA5; 4];
    test_harness::check_eq!(
        client
            .read(opened.fd, 0, &mut bytes)
            .expect("read after refusal"),
        4
    );
    test_harness::check_eq!(bytes, [0; 4]);
    test_harness::check_eq!(
        client
            .stat(opened.fd)
            .wait()
            .expect("length after refusal")
            .size,
        4
    );
    close(&client, &opened);
}

#[test_harness::test]
fn filesystem_failed_file_mapping_aborts_and_can_retry() {
    let client = connect(ram_session());
    let empty = open(
        &client,
        "/mapping-prime",
        AccessMode::Read,
        OpenFlags::CREATE,
    );
    test_harness::check_eq!(
        client
            .read(empty.fd, 0, &mut [0; 1])
            .expect("establish exchange"),
        0
    );
    let opened = open(
        &client,
        "/mapping-retry",
        AccessMode::ReadWrite,
        OpenFlags::CREATE,
    );
    client
        .set_size(opened.fd, 1)
        .wait()
        .expect("create backing");
    let region = Resource::self_resource()
        .expect("resource")
        .create_virtual(USERLAND_PAGE_SIZE, runtime::MemoryAccess::RW)
        .expect("filler backing");
    let mut window = vec![0xA5; filesystem_abi::IO_SCRATCH_BYTES as usize + 1];
    let mut mappings = Vec::new();
    mappings.try_reserve_exact(256).expect("mapping owners");
    loop {
        let handle = region
            .duplicate(Rights::READ | Rights::WRITE, None)
            .expect("filler capability");
        match Mapping::full(handle, UserMemFlags::ReadWrite) {
            Ok(mapping) => mappings.push(mapping),
            Err(_) => break,
        }
    }

    let refused = client.read(opened.fd, 0, &mut window);
    test_harness::check_eq!(refused, Err(CallError::Remote(FileSystemError::NoSpace)));
    drop(mappings.pop());
    test_harness::check_eq!(
        client
            .read(opened.fd, 0, &mut window)
            .expect("retry after releasing mapping slot"),
        1
    );
    test_harness::check_eq!(window.first(), Some(&0));
    drop(mappings);
    close(&client, &opened);
    close(&client, &empty);
}

#[test_harness::test]
fn filesystem_node_maps_new_regions_after_facade_is_dropped() {
    let mut filesystem = FileSystem::new(Session::from_handle(root_view()));
    let mut node = filesystem
        .open(
            Path::new("/tmp/after-facade").expect("path"),
            AccessMode::ReadWrite,
            OpenFlags::CREATE | OpenFlags::TRUNCATE,
        )
        .expect("open");
    drop(filesystem);
    test_harness::check_eq!(node.write(b"head").expect("first mapping"), 4);
    node.seek(USERLAND_PAGE_SIZE);
    test_harness::check_eq!(node.write(b"tail").expect("growth after facade drop"), 4);
    node.seek(USERLAND_PAGE_SIZE);
    let mut bytes = [0; 4];
    test_harness::check_eq!(node.read(&mut bytes).expect("read grown file"), 4);
    test_harness::check_eq!(&bytes, b"tail");
}

#[test_harness::test]
fn filesystem_concurrent_appends_reserve_distinct_offsets() {
    use alloc::sync::Arc;
    use core::sync::atomic::{AtomicU32, AtomicU64, Ordering};

    const APPENDS: u32 = 16;
    const WRITERS: u32 = 16;
    const NAME: &str = "/parallel-append";
    let client = connect(ram_session());
    let opened = open(
        &client,
        NAME,
        AccessMode::ReadWrite,
        OpenFlags::CREATE | OpenFlags::TRUNCATE,
    );
    close(&client, &opened);

    let mut writers = Vec::new();
    let ready = Arc::new(AtomicU32::new(0));
    let gate = Arc::new(Signal::create().expect("writer gate"));
    let offsets: Arc<[AtomicU64]> = (0..WRITERS * APPENDS)
        .map(|_| AtomicU64::new(u64::MAX))
        .collect();
    for group in 0..WRITERS {
        let session = ram_session();
        let ready = ready.clone();
        let gate = gate.clone();
        let offsets = offsets.clone();
        writers.push(
            spawn_owned(move || {
                let client = connect(session);
                let opened = open(&client, NAME, AccessMode::ReadWrite, OpenFlags::NONE);
                for index in 0..APPENDS {
                    let value: u32 = group * APPENDS + index;
                    let written = client
                        .write(opened.fd, APPEND_OFFSET, &value.to_le_bytes())
                        .expect("append");
                    if written.count != 4 || !written.offset.is_multiple_of(4) {
                        return 1;
                    }

                    let Some(offset) = offsets.get(value as usize) else {
                        return 1;
                    };
                    offset.store(written.offset, Ordering::Release);

                    if index == 0 {
                        if ready.fetch_add(1, Ordering::AcqRel) + 1 == WRITERS {
                            gate.notify_all().expect("all writers mapped");
                        }
                        gate.wait(syscall::SIGNALED, JOIN_TIMEOUT)
                            .expect("writer gate opens");
                    }
                }
                close(&client, &opened);
                0
            })
            .expect("writer thread"),
        );
    }

    for writer in writers {
        writer.wait(JOIN_TIMEOUT).expect("writer finishes");
        test_harness::check_eq!(writer.exit_code().expect("writer exit"), 0);
        writer.join(JOIN_TIMEOUT).expect("writer joins");
    }

    let opened = open(&client, NAME, AccessMode::Read, OpenFlags::NONE);
    let mut bytes = [0; WRITERS as usize * APPENDS as usize * 4];
    test_harness::check_eq!(
        client.read(opened.fd, 0, &mut bytes).expect("read appends"),
        bytes.len() as u32
    );
    let info = client.stat(opened.fd).wait().expect("stat ok");
    test_harness::check_eq!(info.size, bytes.len() as u64);

    let at_returned_offsets = (0u32..).zip(offsets.iter()).all(|(value, offset)| {
        let start = offset.load(Ordering::Acquire) as usize;
        bytes.get(start..).and_then(|tail| tail.get(..4)) == Some(&value.to_le_bytes()[..])
    });
    test_harness::check!(at_returned_offsets);
    close(&client, &opened);
}

#[test_harness::test]
fn filesystem_window_timeout_releases_resize_and_other_files_keep_progressing() {
    use alloc::sync::Arc;

    use filesystem_abi::IoPhase;

    let client = connect(ram_session());
    let opened = open(
        &client,
        "/window-timeout",
        AccessMode::ReadWrite,
        OpenFlags::CREATE | OpenFlags::TRUNCATE,
    );
    client.write(opened.fd, 0, b"held").expect("initial bytes");
    let started = Arc::new(Signal::create().expect("resize start signal"));
    let completed = Arc::new(Signal::create().expect("resize completion signal"));
    let mut resizer = None;
    let expired = client.with_window(IoPhase::Read, opened.fd, 0, 4, |_, _, count| {
        let other = connect(ram_session());
        let independent = open(
            &other,
            "/during-window",
            AccessMode::ReadWrite,
            OpenFlags::CREATE | OpenFlags::TRUNCATE,
        );
        other
            .write(independent.fd, 0, b"free")
            .expect("independent write");
        close(&other, &independent);
        let session = ram_session();
        let started_worker = started.clone();
        let completed_worker = completed.clone();
        resizer = Some(
            spawn_owned(move || {
                let other = IoClient::new(session.client().with_wait(Timeout::from_secs(5)));
                let target = open(
                    &other,
                    "/window-timeout",
                    AccessMode::ReadWrite,
                    OpenFlags::NONE,
                );
                started_worker.notify_all().expect("resize begins");
                other
                    .set_size(target.fd, USERLAND_PAGE_SIZE * 2)
                    .wait()
                    .expect("resize after expiry");
                completed_worker.notify_all().expect("resize completed");
                close(&other, &target);
                0
            })
            .expect("resize worker"),
        );
        started
            .wait(syscall::SIGNALED, JOIN_TIMEOUT)
            .expect("resize started");
        assert!(
            completed
                .wait(syscall::SIGNALED, Timeout::from_millis(50))
                .is_err()
        );
        completed
            .wait(syscall::SIGNALED, Timeout::from_secs(5))
            .expect("lease expires");
        Ok(count as u32)
    });
    test_harness::check_eq!(
        expired.err(),
        Some(CallError::Remote(FileSystemError::Unresponsive))
    );
    let resizer = resizer.expect("resize worker created");
    resizer.wait(JOIN_TIMEOUT).expect("resize worker finishes");
    test_harness::check_eq!(resizer.exit_code().expect("resize exit"), 0);
    resizer.join(JOIN_TIMEOUT).expect("resize worker joins");
    test_harness::check_eq!(
        client
            .stat(opened.fd)
            .wait()
            .expect("length after expiry")
            .size,
        USERLAND_PAGE_SIZE * 2
    );
    let observer = connect(ram_session());
    let target = open(
        &observer,
        "/window-timeout",
        AccessMode::Read,
        OpenFlags::NONE,
    );
    let mut bytes = [0; 4];
    observer
        .read(target.fd, 0, &mut bytes)
        .expect("read after expiry");
    test_harness::check_eq!(&bytes, b"held");
    close(&observer, &target);
    close(&client, &opened);
}
