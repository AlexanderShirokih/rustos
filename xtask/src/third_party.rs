//! Реестр закреплённого стороннего кода, из которого собираются программы userland. Каталог
//! записи - `target/third-party/<name>`.

use std::path::{Path, PathBuf};

use anyhow::Result;

use crate::{
    context::project_root,
    fetch::{self, Format, Pin},
};

const DIR: &str = "target/third-party";

struct Entry {
    name: &'static str,
    pin: Pin,
}

const ENTRIES: &[Entry] = &[
    Entry {
        name: "dejavu-fonts",
        pin: Pin::Archive {
            url: "https://github.com/dejavu-fonts/dejavu-fonts/releases/download/version_2_37/dejavu-fonts-ttf-2.37.zip",
            sha256: "7576310b219e04159d35ff61dd4a4ec4cdba4f35c00e002a136f00e96a908b0a",
            format: Format::Zip,
            members: &["dejavu-fonts-ttf-2.37/ttf"],
        },
    },
    Entry {
        name: "doomgeneric",
        pin: Pin::GitCommit {
            repo: "https://github.com/ozkl/doomgeneric",
            commit: "dcb7a8dbc7a16ce3dda29382ac9aae9d77d21284",
        },
    },
    Entry {
        name: "freedoom",
        pin: Pin::Archive {
            url: "https://github.com/freedoom/freedoom/releases/download/v0.13.0/freedoom-0.13.0.zip",
            sha256: "3f9b264f3e3ce503b4fb7f6bdcb1f419d93c7b546f4df3e874dd878db9688f59",
            format: Format::Zip,
            members: &[],
        },
    },
    Entry {
        name: "inter",
        pin: Pin::Archive {
            url: "https://github.com/rsms/inter/releases/download/v4.1/Inter-4.1.zip",
            sha256: "9883fdd4a49d4fb66bd8177ba6625ef9a64aa45899767dde3d36aa425756b11e",
            format: Format::Zip,
            members: &["extras/ttf"],
        },
    },
    Entry {
        name: "sqlite",
        pin: Pin::Archive {
            url: "https://sqlite.org/2026/sqlite-amalgamation-3540000.zip",
            sha256: "68e913b0fe8ec6b4e5f391c594a28872884e80d6cd042a1e50c957be46b5aa4d",
            format: Format::Zip,
            members: &[],
        },
    },
];

pub(crate) fn materialize_sources() -> Result<()> {
    let root = project_root();
    ENTRIES
        .iter()
        .try_for_each(|entry| materialize(&root, entry, false).map(drop))
}

pub(crate) fn run(force: bool) -> Result<()> {
    let root = project_root();
    for entry in ENTRIES {
        let dir = materialize(&root, entry, force)?;
        println!("third-party: {}", dir.display());
    }
    Ok(())
}

fn materialize(root: &Path, entry: &Entry, force: bool) -> Result<PathBuf> {
    let dir = root.join(DIR).join(entry.name);
    fetch::materialize(entry.name, &dir, &entry.pin, force)
}
