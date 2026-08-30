//! Кэш сессий по каталогам. Голова списка - последняя использованная запись.

use alloc::{
    string::{String, ToString},
    vec::Vec,
};

use filesystem_abi::Path;

use crate::{environment::Environment, session::Session};

pub(crate) struct SessionCache<E: Environment> {
    entries: Vec<Entry<E>>,
    capacity: usize,
}

struct Entry<E: Environment> {
    directory: String,
    session: Session<E>,
}

impl<E: Environment> SessionCache<E> {
    pub(crate) fn new(capacity: usize) -> Self {
        Self {
            entries: Vec::new(),
            capacity: capacity.max(1),
        }
    }

    pub(crate) fn promote(&mut self, directory: Path<'_>) -> Option<&Session<E>> {
        let index = self.position(directory)?;
        self.lift(index);
        self.head()
    }

    pub(crate) fn insert(&mut self, directory: Path<'_>, session: Session<E>) {
        if let Some(index) = self.position(directory) {
            if let Some(entry) = self.entries.get_mut(index) {
                entry.session = session;
            }
            self.lift(index);
            return;
        }
        if self.entries.len() >= self.capacity {
            // Хвост списка держит запись, к которой не обращались дольше остальных.
            self.entries.pop();
        }
        self.entries.insert(
            0,
            Entry {
                directory: directory.as_str().to_string(),
                session,
            },
        );
    }

    pub(crate) fn head(&self) -> Option<&Session<E>> {
        self.entries.first().map(|entry| &entry.session)
    }

    pub(crate) fn forget_head(&mut self) {
        if self.entries.is_empty() {
            return;
        }
        self.entries.remove(0);
    }

    fn position(&self, directory: Path<'_>) -> Option<usize> {
        self.entries
            .iter()
            .position(|entry| entry.directory == directory.as_str())
    }

    fn lift(&mut self, index: usize) {
        if index == 0 || index >= self.entries.len() {
            return;
        }
        let entry = self.entries.remove(index);
        self.entries.insert(0, entry);
    }
}
