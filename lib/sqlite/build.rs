//! Компилирует amalgamation sqlite с конфигурацией сборки системы, VFS `molecular` и мьютексы
//! sqlite. Каталог amalgamation с `sqlite3.h` отдаётся зависимым пакетам как
//! `DEP_SQLITE3_INCLUDE`.

use userland_build::{CSources, Error, third_party_dir};

const ENTRY: &str = "sqlite";
const AMALGAMATION_DIR: &str = "sqlite-amalgamation-3540000";

const CONFIGURATION: &[&str] = &[
    "SQLITE_OS_OTHER=1",
    "SQLITE_THREADSAFE=1",
    "SQLITE_OMIT_LOAD_EXTENSION",
    "SQLITE_OMIT_WAL",
    "SQLITE_MAX_MMAP_SIZE=0",
    "SQLITE_TEMP_STORE=3",
];

const VFS: &[&str] = &["src/vfs.c", "src/mutex.c"];

fn main() -> Result<(), Error> {
    let root = third_party_dir(ENTRY)?.join(AMALGAMATION_DIR);
    let amalgamation = root.join("sqlite3.c").display().to_string();
    let sources = [amalgamation.as_str()];

    CONFIGURATION
        .iter()
        .fold(CSources::new("sqlite3", &sources), |sqlite, definition| {
            sqlite.define(definition)
        })
        .foreign()
        .compile()?;
    CSources::new("sqlite-vfs", VFS)
        .include_dir(&root)
        .compile()?;

    println!("cargo:include={}", root.display());
    Ok(())
}
