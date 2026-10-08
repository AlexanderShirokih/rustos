//! E2E scoped-сессий: `open_root` выдаёт сессию-окно узла, и пути её кадров лежат
//! внутри окна. Сужение складывается по цепочке производных, а расширить окно обратно
//! производная сессия не может.

use alloc::{format, string::String};

use filesystem_abi::{
    AccessMode, FileSystemError, NAME_MAX, NodeKind, OpenFlags, PATH_MAX, file_system,
};
use ipc::CallError;
use kernel_objects::{Session, SessionTransport};
use runtime::UserMemFlags;

use crate::filesystem::{
    BUFFER_SIZE, COMPONENT_DIRECTORY, IMAGE_FILE, IMAGE_FILE_BODY, acquire_buffer, buffer_head,
    connect, image_session, open, path, ram_session, walk_directory, write_all,
};

const WRITE_WINDOW: &str = "/scope-write";
const CHAIN_WINDOW: &str = "/scope-chain";
const ROOT_WINDOW: &str = "/scope-root";

fn open_root(
    client: &file_system::Client<SessionTransport>,
    text: &str,
) -> Result<Session<file_system::Protocol>, CallError<FileSystemError>> {
    client.open_root(path(text)).map(Session::from_session)
}

fn window_of(
    client: &file_system::Client<SessionTransport>,
    text: &str,
) -> Session<file_system::Protocol> {
    open_root(client, text).expect("window granted")
}

#[test_harness::test]
fn filesystem_scopes_a_session_to_an_image_directory() {
    let parent = connect(image_session());
    let _parent_buffer = acquire_buffer(&parent);
    let scoped = connect(window_of(&parent, COMPONENT_DIRECTORY));

    let opened = open(&scoped, "/sample", AccessMode::Read, OpenFlags::NONE);
    test_harness::check_eq!(opened.kind, NodeKind::File);
    test_harness::check_eq!(opened.size, IMAGE_FILE_BODY.len() as u64);
    let unread = scoped.read(opened.fd, 0, BUFFER_SIZE);
    test_harness::check_eq!(unread, Err(CallError::Remote(FileSystemError::NoBuffer)));

    let region = acquire_buffer(&scoped);
    let buffer = region
        .map_full(UserMemFlags::ReadWrite)
        .expect("map data buffer");
    let count = scoped.read(opened.fd, 0, BUFFER_SIZE).expect("read ok");
    test_harness::check_eq!(buffer_head(&buffer, count as usize), IMAGE_FILE_BODY);

    let root = open(&scoped, "/", AccessMode::Read, OpenFlags::DIRECTORY);
    let names = walk_directory(&scoped, &buffer, root.fd);
    for expected in ["sample", "exec"] {
        test_harness::check!(names.iter().any(|name| name.as_str() == expected));
    }
}

#[test_harness::test]
fn filesystem_scoped_session_stays_inside_its_window() {
    let parent = connect(image_session());
    let scoped = connect(window_of(&parent, COMPONENT_DIRECTORY));

    let absolute = scoped.open(path(IMAGE_FILE), AccessMode::Read, OpenFlags::NONE);
    test_harness::check_eq!(absolute, Err(CallError::Remote(FileSystemError::NotFound)));

    let escape = scoped.open(path("/../sample"), AccessMode::Read, OpenFlags::NONE);
    test_harness::check_eq!(escape, Err(CallError::Remote(FileSystemError::InvalidPath)));

    let mut tail = String::new();
    while tail.len() + COMPONENT_DIRECTORY.len() <= PATH_MAX {
        let room = PATH_MAX - tail.len() - 1;
        tail.push('/');
        tail.push_str(&"p".repeat(room.min(NAME_MAX)));
    }
    let overflow = scoped.open(path(&tail), AccessMode::Read, OpenFlags::NONE);
    test_harness::check_eq!(
        overflow,
        Err(CallError::Remote(FileSystemError::InvalidPath))
    );

    let unscoped = parent.open(path(&tail), AccessMode::Read, OpenFlags::NONE);
    test_harness::check_eq!(unscoped, Err(CallError::Remote(FileSystemError::NotFound)));
}

#[test_harness::test]
fn filesystem_scoped_windows_narrow_and_compose() {
    let client = connect(ram_session());
    let region = acquire_buffer(&client);
    let buffer = region
        .map_full(UserMemFlags::ReadWrite)
        .expect("map data buffer");

    let outer = format!("{CHAIN_WINDOW}/outer");
    let inner = format!("{outer}/inner");
    for directory in [CHAIN_WINDOW, outer.as_str(), inner.as_str()] {
        open(
            &client,
            directory,
            AccessMode::Read,
            OpenFlags::CREATE | OpenFlags::DIRECTORY,
        );
    }
    let leaf = open(
        &client,
        &format!("{inner}/leaf"),
        AccessMode::ReadWrite,
        OpenFlags::CREATE,
    );
    let payload = b"windowed";
    write_all(&client, &buffer, &leaf, 0, payload);

    let first = connect(window_of(&client, &format!("{CHAIN_WINDOW}/outer")));
    let scoped = connect(window_of(&first, "/inner"));
    let reached = open(&scoped, "/leaf", AccessMode::Read, OpenFlags::NONE);
    test_harness::check_eq!(reached.size, payload.len() as u64);

    let outside = scoped.open(path("/inner/leaf"), AccessMode::Read, OpenFlags::NONE);
    test_harness::check_eq!(outside, Err(CallError::Remote(FileSystemError::NotFound)));

    drop(scoped);
    drop(first);
    let alive = client.stat(leaf.fd).expect("stat ok");
    test_harness::check_eq!(alive.size, payload.len() as u64);
}

#[test_harness::test]
fn filesystem_scoped_session_writes_through_its_window() {
    let client = connect(ram_session());
    open(
        &client,
        WRITE_WINDOW,
        AccessMode::Read,
        OpenFlags::CREATE | OpenFlags::DIRECTORY,
    );

    let scoped = connect(window_of(&client, WRITE_WINDOW));
    let region = acquire_buffer(&scoped);
    let buffer = region
        .map_full(UserMemFlags::ReadWrite)
        .expect("map data buffer");

    let created = open(&scoped, "/fresh", AccessMode::ReadWrite, OpenFlags::CREATE);
    let payload = b"through the window";
    write_all(&scoped, &buffer, &created, 0, payload);
    let outer = open(
        &client,
        &format!("{WRITE_WINDOW}/fresh"),
        AccessMode::Read,
        OpenFlags::NONE,
    );
    test_harness::check_eq!(outer.size, payload.len() as u64);

    scoped
        .rename(path("/fresh"), path("/renamed"))
        .expect("rename ok");
    scoped
        .unlink(path("/renamed"), NodeKind::File)
        .expect("unlink ok");
    let removed = client.open(
        path(&format!("{WRITE_WINDOW}/renamed")),
        AccessMode::Read,
        OpenFlags::NONE,
    );
    test_harness::check_eq!(removed, Err(CallError::Remote(FileSystemError::NotFound)));
}

#[test_harness::test]
fn filesystem_open_root_accepts_any_existing_node() {
    let parent = connect(image_session());

    let file = connect(window_of(&parent, IMAGE_FILE));
    let opened = open(&file, "/", AccessMode::Read, OpenFlags::NONE);
    test_harness::check_eq!(opened.kind, NodeKind::File);
    test_harness::check_eq!(opened.size, IMAGE_FILE_BODY.len() as u64);
    test_harness::check_eq!(
        file.open(path("/child"), AccessMode::Read, OpenFlags::NONE),
        Err(CallError::Remote(FileSystemError::NotADirectory))
    );
    test_harness::check_eq!(
        open_root(&parent, "/absent-window").err(),
        Some(CallError::Remote(FileSystemError::NotFound))
    );

    let scoped = connect(window_of(&parent, "/"));
    let opened = open(&scoped, IMAGE_FILE, AccessMode::Read, OpenFlags::NONE);
    test_harness::check_eq!(opened.size, IMAGE_FILE_BODY.len() as u64);
}

#[test_harness::test]
fn filesystem_scoped_session_cannot_name_its_own_root() {
    let client = connect(ram_session());
    open(
        &client,
        ROOT_WINDOW,
        AccessMode::Read,
        OpenFlags::CREATE | OpenFlags::DIRECTORY,
    );
    let scoped = connect(window_of(&client, ROOT_WINDOW));

    let removed = scoped.unlink(path("/"), NodeKind::Directory);
    test_harness::check_eq!(
        removed,
        Err(CallError::Remote(FileSystemError::InvalidPath))
    );
    let moved = scoped.rename(path("/"), path("/inside"));
    test_harness::check_eq!(moved, Err(CallError::Remote(FileSystemError::InvalidPath)));
    let onto = scoped.rename(path("/inside"), path("/"));
    test_harness::check_eq!(onto, Err(CallError::Remote(FileSystemError::InvalidPath)));

    let alive = open(&client, ROOT_WINDOW, AccessMode::Read, OpenFlags::DIRECTORY);
    test_harness::check_eq!(alive.kind, NodeKind::Directory);

    client
        .unlink(path(ROOT_WINDOW), NodeKind::Directory)
        .expect("unlink ok");
    let recreated = scoped.open(path("/"), AccessMode::ReadWrite, OpenFlags::CREATE);
    test_harness::check_eq!(recreated, Err(CallError::Remote(FileSystemError::NotFound)));
}
