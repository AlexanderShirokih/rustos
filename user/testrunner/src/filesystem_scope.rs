//! E2E scoped-сессий: `open_root` выдаёт сессию-окно узла, и пути её кадров лежат
//! внутри окна. Сужение складывается по цепочке производных, а расширить окно обратно
//! производная сессия не может.

use alloc::{format, string::String};

use filesystem_abi::{AccessMode, FileSystemError, NAME_MAX, NodeKind, OpenFlags, PATH_MAX};
use kernel_objects::OwnedHandle;
use kernel_tests::kernel_test;
use runtime::UserMemFlags;

use crate::filesystem::{
    BUFFER_SIZE, COMPONENT_DIRECTORY, IMAGE_FILE, IMAGE_FILE_BODY, acquire_buffer, buffer_head,
    connect, image_session, open, path, ram_session, walk_directory, write_all,
};

const WRITE_WINDOW: &str = "/scope-write";
const CHAIN_WINDOW: &str = "/scope-chain";
const ROOT_WINDOW: &str = "/scope-root";

fn open_root(capability: &OwnedHandle, text: &str) -> Result<OwnedHandle, FileSystemError> {
    let mut response = connect(capability)
        .open_root(path(text))
        .expect("open_root call");
    let window = response.value?;
    Ok(OwnedHandle::from(
        response.claim(window).expect("capability owner"),
    ))
}

fn window_of(capability: &OwnedHandle, text: &str) -> OwnedHandle {
    open_root(capability, text).expect("window granted")
}

#[kernel_test]
fn filesystem_scopes_a_session_to_an_image_directory() {
    let capability = image_session();
    let parent = connect(&capability);
    let _parent_buffer = acquire_buffer(&parent);
    let window = window_of(&capability, COMPONENT_DIRECTORY);
    let scoped = connect(&window);

    // Буфер данных у производной сессии свой. Буфер родительской сессии ей не достаётся.
    let opened = open(&scoped, "/sample", AccessMode::Read, OpenFlags::NONE);
    kernel_tests::kassert_eq!(opened.kind, NodeKind::File);
    kernel_tests::kassert_eq!(opened.size, IMAGE_FILE_BODY.len() as u64);
    let unread = scoped
        .read(opened.fd, 0, BUFFER_SIZE)
        .expect("read call")
        .value;
    kernel_tests::kassert_eq!(unread, Err(FileSystemError::NoBuffer));

    let region = acquire_buffer(&scoped);
    let buffer = region
        .map_full(UserMemFlags::ReadWrite)
        .expect("map data buffer");
    let count = scoped
        .read(opened.fd, 0, BUFFER_SIZE)
        .expect("read call")
        .value
        .expect("read ok");
    kernel_tests::kassert_eq!(buffer_head(&buffer, count as usize), IMAGE_FILE_BODY);

    // Корень окна - сам каталог: его записи те же, что у каталога родительской сессии.
    let root = open(&scoped, "/", AccessMode::Read, OpenFlags::DIRECTORY);
    let names = walk_directory(&scoped, &buffer, root.fd);
    for expected in ["sample", "program"] {
        kernel_tests::kassert!(names.iter().any(|name| name.as_str() == expected));
    }
}

#[kernel_test]
fn filesystem_scoped_session_stays_inside_its_window() {
    let capability = image_session();
    let window = window_of(&capability, COMPONENT_DIRECTORY);
    let scoped = connect(&window);

    // Путь от корня монтирования внутри окна означает другой узел: /boot/boot/sample
    // в дереве образа отсутствует.
    let absolute = scoped
        .open(path(IMAGE_FILE), AccessMode::Read, OpenFlags::NONE)
        .expect("open call")
        .value;
    kernel_tests::kassert_eq!(absolute, Err(FileSystemError::NotFound));

    // Подъём из окна запрещён формой пути: `..` не проходит правила контракта.
    let escape = scoped
        .open(path("/../sample"), AccessMode::Read, OpenFlags::NONE)
        .expect("open call")
        .value;
    kernel_tests::kassert_eq!(escape, Err(FileSystemError::InvalidPath));

    // Границы PATH_MAX принадлежат полному пути. Хвост в границах кадра, но окно
    // удлиняет его за предел.
    let mut tail = String::new();
    while tail.len() + COMPONENT_DIRECTORY.len() <= PATH_MAX {
        let room = PATH_MAX - tail.len() - 1;
        tail.push('/');
        tail.push_str(&"p".repeat(room.min(NAME_MAX)));
    }
    let overflow = scoped
        .open(path(&tail), AccessMode::Read, OpenFlags::NONE)
        .expect("open call")
        .value;
    kernel_tests::kassert_eq!(overflow, Err(FileSystemError::InvalidPath));

    // Тот же хвост без окна валиден. Отказ выше - про границу, а не про форму.
    let unscoped = connect(&capability)
        .open(path(&tail), AccessMode::Read, OpenFlags::NONE)
        .expect("open call")
        .value;
    kernel_tests::kassert_eq!(unscoped, Err(FileSystemError::NotFound));
}

#[kernel_test]
fn filesystem_scoped_windows_narrow_and_compose() {
    let capability = ram_session();
    let client = connect(&capability);
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

    // Сужение складывается. Окно, открытое из окна, отсчитывается от него.
    let first = window_of(&capability, &format!("{CHAIN_WINDOW}/outer"));
    let second = window_of(&first, "/inner");
    let scoped = connect(&second);
    let reached = open(&scoped, "/leaf", AccessMode::Read, OpenFlags::NONE);
    kernel_tests::kassert_eq!(reached.size, payload.len() as u64);

    // Путь соседнего уровня в узком окне не существует: расширить окно обратно нечем.
    let outside = scoped
        .open(path("/inner/leaf"), AccessMode::Read, OpenFlags::NONE)
        .expect("open call")
        .value;
    kernel_tests::kassert_eq!(outside, Err(FileSystemError::NotFound));

    // Уход производных сессий не трогает родительскую: её дескрипторы живы.
    drop(second);
    drop(first);
    let alive = client
        .stat(leaf.fd)
        .expect("stat call")
        .value
        .expect("stat ok");
    kernel_tests::kassert_eq!(alive.size, payload.len() as u64);
}

#[kernel_test]
fn filesystem_scoped_session_writes_through_its_window() {
    let capability = ram_session();
    let client = connect(&capability);
    open(
        &client,
        WRITE_WINDOW,
        AccessMode::Read,
        OpenFlags::CREATE | OpenFlags::DIRECTORY,
    );

    let window = window_of(&capability, WRITE_WINDOW);
    let scoped = connect(&window);
    let region = acquire_buffer(&scoped);
    let buffer = region
        .map_full(UserMemFlags::ReadWrite)
        .expect("map data buffer");

    // Окно - не read-only срез: запись сессии-окна видна родительскому дереву.
    let created = open(&scoped, "/fresh", AccessMode::ReadWrite, OpenFlags::CREATE);
    let payload = b"through the window";
    write_all(&scoped, &buffer, &created, 0, payload);
    let outer = open(
        &client,
        &format!("{WRITE_WINDOW}/fresh"),
        AccessMode::Read,
        OpenFlags::NONE,
    );
    kernel_tests::kassert_eq!(outer.size, payload.len() as u64);

    // Переименование и удаление тоже пересаживаются под окно.
    scoped
        .rename(path("/fresh"), path("/renamed"))
        .expect("rename call")
        .value
        .expect("rename ok");
    scoped
        .unlink(path("/renamed"), NodeKind::File)
        .expect("unlink call")
        .value
        .expect("unlink ok");
    let removed = client
        .open(
            path(&format!("{WRITE_WINDOW}/renamed")),
            AccessMode::Read,
            OpenFlags::NONE,
        )
        .expect("open call")
        .value;
    kernel_tests::kassert_eq!(removed, Err(FileSystemError::NotFound));
}

#[kernel_test]
fn filesystem_open_root_accepts_any_existing_node() {
    let capability = image_session();

    let file_window = window_of(&capability, IMAGE_FILE);
    let file = connect(&file_window);
    let opened = open(&file, "/", AccessMode::Read, OpenFlags::NONE);
    kernel_tests::kassert_eq!(opened.kind, NodeKind::File);
    kernel_tests::kassert_eq!(opened.size, IMAGE_FILE_BODY.len() as u64);
    kernel_tests::kassert_eq!(
        file.open(path("/child"), AccessMode::Read, OpenFlags::NONE)
            .expect("open call")
            .value,
        Err(FileSystemError::NotADirectory)
    );
    kernel_tests::kassert_eq!(
        open_root(&capability, "/absent-window").err(),
        Some(FileSystemError::NotFound)
    );

    // Корень как окно - то же дерево. Производная сессия получает его целиком.
    let window = window_of(&capability, "/");
    let scoped = connect(&window);
    let opened = open(&scoped, IMAGE_FILE, AccessMode::Read, OpenFlags::NONE);
    kernel_tests::kassert_eq!(opened.size, IMAGE_FILE_BODY.len() as u64);
}

#[kernel_test]
fn filesystem_scoped_session_cannot_name_its_own_root() {
    let capability = ram_session();
    let client = connect(&capability);
    open(
        &client,
        ROOT_WINDOW,
        AccessMode::Read,
        OpenFlags::CREATE | OpenFlags::DIRECTORY,
    );
    let window = window_of(&capability, ROOT_WINDOW);
    let scoped = connect(&window);

    // Корень окна - запись каталога над ним. Операция над именем его не называет.
    let removed = scoped
        .unlink(path("/"), NodeKind::Directory)
        .expect("unlink call")
        .value;
    kernel_tests::kassert_eq!(removed, Err(FileSystemError::InvalidPath));
    let moved = scoped
        .rename(path("/"), path("/inside"))
        .expect("rename call")
        .value;
    kernel_tests::kassert_eq!(moved, Err(FileSystemError::InvalidPath));
    let onto = scoped
        .rename(path("/inside"), path("/"))
        .expect("rename call")
        .value;
    kernel_tests::kassert_eq!(onto, Err(FileSystemError::InvalidPath));

    // Отказы не тронули дерево родителя: каталог окна на месте.
    let alive = open(&client, ROOT_WINDOW, AccessMode::Read, OpenFlags::DIRECTORY);
    kernel_tests::kassert_eq!(alive.kind, NodeKind::Directory);

    // Корень окна сессия открывает, но не создаёт. Снятый родителем каталог не возвращается.
    client
        .unlink(path(ROOT_WINDOW), NodeKind::Directory)
        .expect("unlink call")
        .value
        .expect("unlink ok");
    let recreated = scoped
        .open(path("/"), AccessMode::ReadWrite, OpenFlags::CREATE)
        .expect("open call")
        .value;
    kernel_tests::kassert_eq!(recreated, Err(FileSystemError::NotFound));
}
