//! Обработчики глобала `Desktop`: выбор, приём сканирований и навигация клавишами.

use core::cell::RefCell;
use std::{
    rc::Rc,
    sync::{Arc, Mutex, mpsc},
    thread,
};

use slint::ComponentHandle;

use crate::{
    navigation::{Focus, Selection},
    scan::{Scan, scan_files},
    ui::{self, Level},
};

pub(crate) fn start_browsing(files: &ui::Files, mounts: Vec<&'static str>) {
    let initial = Selection::new(mounts.first().copied().unwrap_or("/"), "", "");
    let selection = Arc::new(Mutex::new(initial.clone()));
    let focus = Rc::new(RefCell::new(Focus::new(Level::Mounts, initial.clone())));
    let (rescan, rescan_requests) = mpsc::channel();
    let (scans, inbox) = mpsc::channel::<Scan>();
    let desktop = files.global::<ui::Desktop<'_>>();
    desktop.set_level(Level::Mounts);
    desktop.set_mount(initial.mount.into());

    let weak = files.as_weak();
    let chosen = selection.clone();
    let selected = focus.clone();
    desktop.on_select(move |level, mount, folder, file| {
        if let Some(files) = weak.upgrade() {
            let desktop = files.global::<ui::Desktop<'_>>();
            desktop.set_level(level);
            desktop.set_mount(mount.clone());
            desktop.set_folder(folder.clone());
            desktop.set_file(file.clone());
        }
        let selection = Selection::new(&mount, &folder, &file);
        if let Ok(mut focus) = selected.try_borrow_mut() {
            focus.level = level;
            focus.selection = selection.clone();
        }
        if let Ok(mut chosen) = chosen.lock() {
            *chosen = selection;
        }
        let _ = rescan.send(());
    });

    let weak = files.as_weak();
    let scanned = focus.clone();
    desktop.on_receive(move || {
        let Some(files) = weak.upgrade() else {
            return;
        };
        while let Ok(scan) = inbox.try_recv() {
            if let Ok(mut focus) = scanned.try_borrow_mut() {
                focus.set_listing(scan.listing());
            }
            scan.show(&files);
        }
    });

    let weak = files.as_weak();
    desktop.on_navigate(move |step| {
        let target = focus
            .try_borrow()
            .ok()
            .and_then(|focus| focus.navigate(&mounts, step));

        if let (Some(files), Some((level, selection))) = (weak.upgrade(), target) {
            files.global::<ui::Desktop<'_>>().invoke_select(
                level,
                selection.mount.into(),
                selection.folder.into(),
                selection.file.into(),
            );
        }
    });

    let weak = files.as_weak();
    thread::spawn(move || scan_files(&weak, &selection, &rescan_requests, &scans));
}
