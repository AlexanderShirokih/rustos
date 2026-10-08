//! Программа читает файл из userland-образа, создаёт
//! каталог под `/tmp`, пишет и читает файл, перечисляет каталог через маршрутизатор имён.

use core::error::Error;

use components::component_manifest;
use filesystem_abi::{AccessMode, OpenFlags, Path, const_path, namespace};
use filesystem_client::{Failure, FileSystem, FileSystemEnvironment, Listing};

const NOTES_DIRECTORY: Path<'static> = const_path!("/tmp/files-demo");

component_manifest! {
    struct FilesStartup {
        requirements: {
            namespace: session(namespace::Protocol),
        },
    }
}

fn main() -> Result<(), Box<dyn Error>> {
    let namespace = components::requirements::<FilesStartup>()?
        .namespace
        .open()?;
    println!("started");

    let mut files = FileSystem::new(namespace);

    print_greeting(&mut files)?;
    test_tmp_filesystem(&mut files)?;

    println!("done");
    Ok(())
}

fn print_greeting(files: &mut FileSystem<FileSystemEnvironment>) -> Result<(), Failure> {
    let mut greeting = files.open(
        const_path!("/boot/files-demo/greeting.txt"),
        AccessMode::Read,
        OpenFlags::NONE,
    )?;

    let mut bytes = [0; 64];
    let count = greeting.read(&mut bytes)?;
    println!("greeting text: {}", text(bytes.get(..count)));

    Ok(())
}

fn test_tmp_filesystem(namespace: &mut FileSystem<FileSystemEnvironment>) -> Result<(), Failure> {
    namespace.create_directory(NOTES_DIRECTORY)?;

    println!("tmp directory: {}", NOTES_DIRECTORY.as_str());

    let note_path = NOTES_DIRECTORY.join_segment("note.txt")?;
    let note_file = note_path.as_path();

    let mut note = namespace.open(
        note_file,
        AccessMode::Write,
        OpenFlags::CREATE | OpenFlags::TRUNCATE,
    )?;

    let written = note.write(b"written through /tmp")?;
    println!("tmp write: {written} bytes");
    drop(note);

    let mut note = namespace.open(note_file, AccessMode::Read, OpenFlags::NONE)?;
    let mut bytes = [0; 64];
    let count = note.read(&mut bytes)?;
    println!("tmp read: {}", text(bytes.get(..count)));

    print_entries(namespace.read_directory(NOTES_DIRECTORY)?)
}

fn print_entries(mut listing: Listing<FileSystemEnvironment>) -> Result<(), Failure> {
    while let Some(entry) = listing.next_entry()? {
        println!("entry: {} {:?}", entry.name, entry.kind);
    }
    Ok(())
}

fn text(bytes: Option<&[u8]>) -> &str {
    bytes
        .and_then(|bytes| str::from_utf8(bytes).ok())
        .map_or("<not a text>", str::trim_end)
}
