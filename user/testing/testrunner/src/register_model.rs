//! Хранилище 32-битных регистров модели устройства с журналом обращений.

use alloc::{collections::BTreeMap, vec::Vec};
use core::cell::RefCell;

pub(crate) struct RegisterFile {
    values: RefCell<BTreeMap<(usize, usize), u32>>,
    accesses: RefCell<Vec<Access>>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Access {
    Load {
        window: usize,
        offset: usize,
    },
    Store {
        window: usize,
        offset: usize,
        value: u32,
    },
}

impl RegisterFile {
    pub(crate) fn new() -> Self {
        Self {
            values: RefCell::new(BTreeMap::new()),
            accesses: RefCell::new(Vec::new()),
        }
    }

    pub(crate) fn get(&self, window: usize, offset: usize) -> u32 {
        self.values
            .borrow()
            .get(&(window, offset))
            .copied()
            .unwrap_or(0)
    }

    pub(crate) fn set(&self, window: usize, offset: usize, value: u32) {
        self.values.borrow_mut().insert((window, offset), value);
    }

    pub(crate) fn load(&self, window: usize, offset: usize) -> u32 {
        self.accesses
            .borrow_mut()
            .push(Access::Load { window, offset });
        self.get(window, offset)
    }

    pub(crate) fn store(&self, window: usize, offset: usize, value: u32) {
        self.accesses.borrow_mut().push(Access::Store {
            window,
            offset,
            value,
        });
        self.set(window, offset, value);
    }

    pub(crate) fn take_accesses(&self) -> Vec<Access> {
        self.accesses.take()
    }
}
