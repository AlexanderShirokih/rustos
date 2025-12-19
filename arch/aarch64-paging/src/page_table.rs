use crate::entry::{Entry, Kind};
use crate::level::Level;
use core::marker::PhantomData;

pub struct PageTable<L: Level> {
    entries: [u64; 512],
    _p: PhantomData<L>,
}

impl<L: Level> PageTable<L> {
    pub const fn new() -> Self {
        Self {
            entries: [0; 512],
            _p: PhantomData,
        }
    }

    pub fn set<K: Kind>(&mut self, idx: usize, e: Entry<L, K>) {
        self.entries[idx] = e.raw();
    }

    pub fn get_raw(&self, idx: usize) -> u64 {
        self.entries[idx]
    }
}
