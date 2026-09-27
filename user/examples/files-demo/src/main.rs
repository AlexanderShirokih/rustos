//! Демонстрационный компонент файловой подсистемы: читатель собственного каталога и клиент
//! маршрутизатора имён.
//!
//! Через окно своего каталога программа перечисляет его записи, читает запись `/greeting` и
//! отображает регион её содержимого. Через маршрутизатор имён она создаёт каталог под `/tmp`,
//! пишет в нём файл, читает его и перечисляет каталог. Каждая операция пишет строку в журнал.
//!
//! Отказ в окне каталога и отказ под маршрутизатором программа называет разными кодами выхода.

use std::{fmt, process};

use component_startup::component_manifest;
use filesystem_abi::{AccessMode, OpenFlags, Path, const_path, file_system, namespace};
use filesystem_client::{Failure, FileSystem, FileSystemEnvironment, Listing};
use kernel_objects::MemoryRegion;
use syscall::Timeout;

const CALL_TIMEOUT: Timeout = Timeout::from_secs(2);

const AREA_ROOT: Path<'static> = const_path!("/");
const GREETING_RECORD: Path<'static> = const_path!("/greeting");
const NOTES_DIRECTORY: Path<'static> = const_path!("/tmp/files-demo");
const NOTE_RECORD: Path<'static> = const_path!("/tmp/files-demo/note");

const NOTE_TEXT: &str = "written through /tmp";

const TEXT_MAX_BYTES: usize = 64;

const NO_STARTUP_EXIT_CODE: i32 = 1;
const AREA_FAILED_EXIT_CODE: i32 = 2;
const NAMESPACE_FAILED_EXIT_CODE: i32 = 3;

component_manifest! {
    struct FilesStartup {
        requirements: {
            area: session(file_system::Protocol),
            namespace: session(namespace::Protocol),
        },
    }
}

#[derive(Debug)]
enum Refusal {
    Files(Failure),
    Region(kernel_objects::Error),
}

impl fmt::Display for Refusal {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Files(failure) => write!(formatter, "{failure:?}"),
            Self::Region(error) => write!(formatter, "region {error:?}"),
        }
    }
}

impl From<Failure> for Refusal {
    fn from(failure: Failure) -> Self {
        Self::Files(failure)
    }
}

impl From<kernel_objects::Error> for Refusal {
    fn from(error: kernel_objects::Error) -> Self {
        Self::Region(error)
    }
}

fn main() {
    // Без журнала вывод `println!` теряется, и отказ старта передаётся кодом выхода.
    let Ok(requirements) = component_startup_std::start(FilesStartup::start) else {
        process::exit(NO_STARTUP_EXIT_CODE);
    };
    println!("started");

    let mut area = FileSystem::windowed(requirements.area).with_wait(CALL_TIMEOUT);
    if let Err(refusal) = show_area(&mut area) {
        println!("area failed: {refusal}");
        process::exit(AREA_FAILED_EXIT_CODE);
    }

    let mut namespace = FileSystem::routed(requirements.namespace).with_wait(CALL_TIMEOUT);
    if let Err(refusal) = show_namespace(&mut namespace) {
        println!("namespace failed: {refusal}");
        process::exit(NAMESPACE_FAILED_EXIT_CODE);
    }
    println!("done");
}

fn show_area(area: &mut FileSystem<FileSystemEnvironment>) -> Result<(), Refusal> {
    // Область windowed-сессии совпадает с каталогом компонента, и пути отсчитываются от его
    // корня.
    print_entries("area", area.read_directory(AREA_ROOT)?)?;

    let mut greeting = area.open(GREETING_RECORD, AccessMode::Read, OpenFlags::NONE)?;
    let mut bytes = [0; TEXT_MAX_BYTES];
    let count = greeting.read(&mut bytes)?;
    println!("area read: {}", text(bytes.get(..count)));

    // Регион покрывает целые страницы, и длину содержимого задаёт размер узла.
    let region = MemoryRegion::from(greeting.backing_memory()?);
    let mapping = region.map_full_read_only()?;
    let length = usize::try_from(greeting.size()).unwrap_or(usize::MAX);
    println!(
        "area backing: {} bytes mapped, {}",
        mapping.size_bytes(),
        text(mapping.as_bytes().get(..length))
    );
    Ok(())
}

fn show_namespace(namespace: &mut FileSystem<FileSystemEnvironment>) -> Result<(), Refusal> {
    namespace.create_directory(NOTES_DIRECTORY)?;
    println!("tmp directory: {}", NOTES_DIRECTORY.as_str());

    let mut note = namespace.open(
        NOTE_RECORD,
        AccessMode::Write,
        OpenFlags::CREATE | OpenFlags::TRUNCATE,
    )?;
    let written = note.write(NOTE_TEXT.as_bytes())?;
    println!("tmp write: {written} bytes");
    drop(note);

    let mut note = namespace.open(NOTE_RECORD, AccessMode::Read, OpenFlags::NONE)?;
    let mut bytes = [0; TEXT_MAX_BYTES];
    let count = note.read(&mut bytes)?;
    println!("tmp read: {}", text(bytes.get(..count)));

    print_entries("tmp", namespace.read_directory(NOTES_DIRECTORY)?)?;
    Ok(())
}

fn print_entries(label: &str, mut listing: Listing<FileSystemEnvironment>) -> Result<(), Refusal> {
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
