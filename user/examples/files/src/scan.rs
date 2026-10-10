//! Сканирование выбора: записи корня тома и выбранного каталога и начало выбранного файла.

use core::time::Duration;
use std::{
    fs,
    io::{self, Read},
    sync::{
        Mutex,
        mpsc::{Receiver, RecvTimeoutError, Sender},
    },
};

use slint::{ComponentHandle, ModelRc, VecModel};

use crate::{
    navigation::{Listing, Selection},
    ui,
};

const RESCAN_INTERVAL: Duration = Duration::from_secs(1);

const FILE_LINES: usize = 34;
const PREVIEW_BYTES_MAX: u64 = 4096;

pub(crate) struct Scan {
    selection: Selection,
    root: Vec<ui::Entry>,
    folder: Vec<ui::Entry>,
    text: String,
}

impl Scan {
    fn collect(selection: &Selection) -> io::Result<Self> {
        let folder = child_path(&selection.mount, &selection.folder);
        Ok(Self {
            selection: selection.clone(),
            root: entries(&selection.mount)?,
            folder: if selection.folder.is_empty() {
                Vec::new()
            } else {
                entries(&folder)?
            },
            text: if selection.file.is_empty() {
                String::new()
            } else {
                preview(&child_path(&folder, &selection.file))?
            },
        })
    }

    pub(crate) fn listing(&self) -> Listing {
        let names = |entries: &[ui::Entry], folders: bool| {
            entries
                .iter()
                .filter(|entry| entry.folder == folders)
                .map(|entry| entry.name.to_string())
                .collect()
        };
        Listing {
            selection: self.selection.clone(),
            folders: names(&self.root, true),
            files: names(&self.folder, false),
        }
    }

    pub(crate) fn show(self, files: &ui::Files) {
        let lines: Vec<ui::Line> = self
            .text
            .lines()
            .take(FILE_LINES)
            .zip(1..)
            .map(|(line, number)| text_line(line, number))
            .collect();

        let desktop = files.global::<ui::Desktop<'_>>();
        desktop.set_root_entries(ModelRc::new(VecModel::from(self.root)));
        desktop.set_folder_entries(ModelRc::new(VecModel::from(self.folder)));
        desktop.set_file_lines(ModelRc::new(VecModel::from(lines)));
    }
}

pub(crate) fn scan_files(
    files: &slint::Weak<ui::Files>,
    selection: &Mutex<Selection>,
    rescan_requests: &Receiver<()>,
    scans: &Sender<Scan>,
) {
    loop {
        let chosen = match selection.lock() {
            Ok(chosen) => chosen.clone(),
            Err(_) => return,
        };
        let posted = match Scan::collect(&chosen) {
            Ok(scan) => {
                scans.send(scan).is_ok()
                    && files
                        .upgrade_in_event_loop(|files| {
                            files.global::<ui::Desktop<'_>>().invoke_receive();
                        })
                        .is_ok()
            }
            Err(error) => {
                let line = text_line(&format!("unavailable: {error}"), 1);
                files
                    .upgrade_in_event_loop(move |files| {
                        files
                            .global::<ui::Desktop<'_>>()
                            .set_file_lines(ModelRc::new(VecModel::from(vec![line])));
                    })
                    .is_ok()
            }
        };
        if !posted {
            return;
        }
        if rescan_requests.recv_timeout(RESCAN_INTERVAL) == Err(RecvTimeoutError::Disconnected) {
            return;
        }
    }
}

fn entries(path: &str) -> io::Result<Vec<ui::Entry>> {
    let mut entries = fs::read_dir(path)?
        .map(|entry| {
            let entry = entry?;
            let metadata = entry.metadata()?;
            Ok(ui::Entry {
                name: entry.file_name().to_string_lossy().into_owned().into(),
                size: if metadata.is_dir() {
                    String::new()
                } else {
                    size_text(metadata.len())
                }
                .into(),
                folder: metadata.is_dir(),
            })
        })
        .collect::<io::Result<Vec<_>>>()?;
    // Каталоги идут первыми, внутри групп записи упорядочены по имени.
    entries.sort_by(|left, right| {
        right
            .folder
            .cmp(&left.folder)
            .then_with(|| left.name.cmp(&right.name))
    });
    Ok(entries)
}

fn child_path(parent: &str, name: &str) -> String {
    format!("{}/{name}", parent.trim_end_matches('/'))
}

fn preview(path: &str) -> io::Result<String> {
    let mut bytes = Vec::new();
    fs::File::open(path)?
        .take(PREVIEW_BYTES_MAX)
        .read_to_end(&mut bytes)?;
    match String::from_utf8(bytes) {
        Ok(text) => Ok(text),
        Err(error) if error.utf8_error().error_len().is_none() => {
            let valid = error.utf8_error().valid_up_to();
            let mut bytes = error.into_bytes();
            bytes.truncate(valid);
            Ok(String::from_utf8(bytes).unwrap_or_default())
        }
        Err(_) => Ok("binary file".into()),
    }
}

fn text_line(line: &str, number: usize) -> ui::Line {
    ui::Line {
        number: number.to_string().into(),
        text: line.into(),
    }
}

fn size_text(bytes: u64) -> String {
    const KIB: u64 = 1024;
    const MIB: u64 = KIB * KIB;

    if bytes < KIB {
        format!("{bytes} B")
    } else if bytes < MIB {
        format!("{}.{} KiB", bytes / KIB, bytes % KIB * 10 / KIB)
    } else {
        format!("{}.{} MiB", bytes / MIB, bytes % MIB * 10 / MIB)
    }
}
