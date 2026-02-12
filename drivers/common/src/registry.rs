extern crate alloc;

use crate::Driver;
use alloc::boxed::Box;
use alloc::vec::Vec;

pub struct RuntimeDriverRegistry {
    drivers: Vec<RunningDriver>,
}

pub struct RunningDriver {
    pub name: &'static str,
    pub driver: Box<dyn Driver>,
}

impl RuntimeDriverRegistry {
    pub fn new() -> Self {
        Self {
            drivers: Vec::new(),
        }
    }

    pub fn insert(&mut self, name: &'static str, driver: Box<dyn Driver>) {
        self.drivers.push(RunningDriver { name, driver });
    }

    pub fn len(&self) -> usize {
        self.drivers.len()
    }

    pub fn is_empty(&self) -> bool {
        self.drivers.is_empty()
    }
}

impl Default for RuntimeDriverRegistry {
    fn default() -> Self {
        Self::new()
    }
}
