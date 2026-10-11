//! Выбор тома, каталога и файла, уровень фокуса и новый выбор по шагу навигации.
//!
//! Уровень тома переходит по томам, уровень каталога - по каталогам корня тома, уровень
//! файла - по файлам выбранного каталога. Списки каталогов и файлов берутся из последнего
//! сканирования и действуют, пока выбор совпадает с выбором, для которого они собраны.

use crate::ui::{Level, Step};

#[derive(Clone)]
pub(crate) struct Selection {
    pub(crate) mount: String,
    pub(crate) folder: String,
    pub(crate) file: String,
}

pub(crate) struct Focus {
    pub(crate) level: Level,
    pub(crate) selection: Selection,
    listing: Option<Listing>,
}

pub(crate) struct Listing {
    pub(crate) selection: Selection,
    pub(crate) folders: Vec<String>,
    pub(crate) files: Vec<String>,
}

impl Selection {
    pub(crate) fn new(mount: &str, folder: &str, file: &str) -> Self {
        Self {
            mount: mount.into(),
            folder: folder.into(),
            file: file.into(),
        }
    }

    fn name(&self, level: Level) -> &str {
        match level {
            Level::Mounts => &self.mount,
            Level::Folder => &self.folder,
            Level::File => &self.file,
        }
    }

    fn with(&self, level: Level, name: &str) -> Self {
        match level {
            Level::Mounts => Self::new(name, "", ""),
            Level::Folder => Self::new(&self.mount, name, ""),
            Level::File => Self::new(&self.mount, &self.folder, name),
        }
    }
}

impl Focus {
    pub(crate) const fn new(level: Level, selection: Selection) -> Self {
        Self {
            level,
            selection,
            listing: None,
        }
    }

    pub(crate) fn set_listing(&mut self, listing: Listing) {
        self.listing = Some(listing);
    }

    pub(crate) fn navigate(&self, mounts: &[&str], step: Step) -> Option<(Level, Selection)> {
        let current = &self.selection;
        match step {
            Step::Next | Step::Previous => {
                let names = self.names(self.level, mounts)?;
                let name = neighbour(&names, current.name(self.level), step)?;
                Some((self.level, current.with(self.level, name)))
            }
            Step::Forward => {
                let level = deeper(self.level)?;
                if !current.name(level).is_empty() {
                    return Some((level, current.clone()));
                }
                let name = self.names(level, mounts)?.into_iter().next()?;
                Some((level, current.with(level, name)))
            }
            Step::Back => Some((shallower(self.level)?, current.clone())),
        }
    }

    fn names<'a>(&'a self, level: Level, mounts: &'a [&'a str]) -> Option<Vec<&'a str>> {
        if level == Level::Mounts {
            return Some(mounts.to_vec());
        }
        let listing = self.listing.as_ref()?;
        if listing.selection.mount != self.selection.mount {
            return None;
        }
        if level == Level::Folder {
            return Some(listing.folders.iter().map(String::as_str).collect());
        }
        (listing.selection.folder == self.selection.folder)
            .then(|| listing.files.iter().map(String::as_str).collect())
    }
}

const fn deeper(level: Level) -> Option<Level> {
    match level {
        Level::Mounts => Some(Level::Folder),
        Level::Folder => Some(Level::File),
        Level::File => None,
    }
}

const fn shallower(level: Level) -> Option<Level> {
    match level {
        Level::Mounts => None,
        Level::Folder => Some(Level::Mounts),
        Level::File => Some(Level::Folder),
    }
}

fn neighbour<'a>(names: &[&'a str], current: &str, step: Step) -> Option<&'a str> {
    let position = names.iter().position(|name| *name == current);
    let index = match (step, position) {
        (Step::Next, Some(index)) => index.checked_add(1)?,
        (Step::Previous, Some(index)) => index.checked_sub(1)?,
        (Step::Next | Step::Previous, None) => 0,
        (Step::Forward | Step::Back, _) => return None,
    };
    names.get(index).copied()
}
