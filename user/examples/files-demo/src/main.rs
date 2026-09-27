//! Демонстрационный компонент файловой подсистемы: читатель собственного каталога и клиент
//! маршрутизатора имён.
//!
//! Через окно своего каталога программа перечисляет его записи и читает запись `/greeting`.
//! Через маршрутизатор имён она создаёт каталог под `/tmp`, пишет в нём файл, читает его и
//! перечисляет каталог. Каждая операция пишет строку в журнал.

use core::error::Error;

use components::component_manifest;
use filesystem_abi::{AccessMode, OpenFlags, Path, const_path, file_system, namespace};
use filesystem_client::{Failure, FileSystem, FileSystemEnvironment, Listing};

const NOTES_DIRECTORY: Path<'static> = const_path!("/tmp/files-demo");
const NOTE_RECORD: Path<'static> = const_path!("/tmp/files-demo/note");

component_manifest! {
    struct FilesStartup {
        requirements: {
            area: session(file_system::Protocol),
            namespace: session(namespace::Protocol),
        },
    }
}

fn main() -> Result<(), Box<dyn Error>> {
    let requirements = components::requirements(FilesStartup::request)?;
    println!("started");

    // `windowed` открывает одну файловую систему, выданную компоненту по `bind` и `scope` плана.
    // Путь отсчитывается от корня выданной области, узлы вне области недоступны. `routed`
    // открывает маршрутизатор имён, и абсолютный путь разбирается по монтированиям системы
    // (`/boot`, `/tmp`).
    show_area(&mut FileSystem::windowed(requirements.area))?;
    show_namespace(&mut FileSystem::routed(requirements.namespace))?;
    println!("done");
    Ok(())
}

fn show_area(area: &mut FileSystem<FileSystemEnvironment>) -> Result<(), Failure> {
    print_entries("area", area.read_directory(const_path!("/"))?)?;

    let mut greeting = area.open(const_path!("/greeting"), AccessMode::Read, OpenFlags::NONE)?;
    let mut bytes = [0; 64];
    let count = greeting.read(&mut bytes)?;
    println!("area read: {}", text(bytes.get(..count)));
    Ok(())
}

fn show_namespace(namespace: &mut FileSystem<FileSystemEnvironment>) -> Result<(), Failure> {
    namespace.create_directory(NOTES_DIRECTORY)?;
    println!("tmp directory: {}", NOTES_DIRECTORY.as_str());

    let mut note = namespace.open(
        NOTE_RECORD,
        AccessMode::Write,
        OpenFlags::CREATE | OpenFlags::TRUNCATE,
    )?;
    let written = note.write(b"written through /tmp")?;
    println!("tmp write: {written} bytes");
    drop(note);

    let mut note = namespace.open(NOTE_RECORD, AccessMode::Read, OpenFlags::NONE)?;
    let mut bytes = [0; 64];
    let count = note.read(&mut bytes)?;
    println!("tmp read: {}", text(bytes.get(..count)));

    print_entries("tmp", namespace.read_directory(NOTES_DIRECTORY)?)
}

fn print_entries(label: &str, mut listing: Listing<FileSystemEnvironment>) -> Result<(), Failure> {
    while let Some(entry) = listing.next_entry()? {
        println!("{label} entry: {} {:?}", entry.name, entry.kind);
    }
    Ok(())
}

fn text(bytes: Option<&[u8]>) -> &str {
    bytes
        .and_then(|bytes| str::from_utf8(bytes).ok())
        .map_or("<not text>", str::trim_end)
}
