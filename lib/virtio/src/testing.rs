use core::cell::RefCell;
use std::{collections::HashMap, rc::Rc};

use crate::{
    access::{DeviceMemory, OutOfRange, Registers},
    transport::{register, status},
};

pub(crate) const REGION_ADDRESS: u64 = 0x1_4000_0000;

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum Event {
    Read(usize),
    Write(usize, u32),
    Barrier,
    MemoryRead(usize),
    MemoryWrite(usize, Vec<u8>),
}

#[derive(Clone, Default)]
pub(crate) struct Log(Rc<RefCell<Vec<Event>>>);

pub(crate) struct FakeRegisters {
    values: HashMap<usize, u32>,
    features: u64,
    accepts_features: bool,
    completes_reset: bool,
    log: Log,
}

pub(crate) struct FakeMemory {
    bytes: Vec<u8>,
    address: u64,
    log: Log,
}

impl Log {
    pub(crate) fn events(&self) -> Vec<Event> {
        self.0.borrow().clone()
    }

    pub(crate) fn writes(&self) -> Vec<Event> {
        let events = self.events();
        events
            .into_iter()
            .filter(|event| matches!(event, Event::Write(..)))
            .collect()
    }

    pub(crate) fn writes_to(&self, offset: usize) -> Vec<u32> {
        let events = self.events();
        events
            .into_iter()
            .filter_map(|event| match event {
                Event::Write(at, value) if at == offset => Some(value),
                _ => None,
            })
            .collect()
    }

    pub(crate) fn clear(&self) {
        self.0.borrow_mut().clear();
    }

    fn push(&self, event: Event) {
        self.0.borrow_mut().push(event);
    }
}

impl FakeRegisters {
    pub(crate) fn gpu(log: &Log) -> Self {
        let values = HashMap::from([
            (register::MAGIC_VALUE, 0x7472_6976),
            (register::VERSION, 2),
            (register::DEVICE_ID, 16),
            (register::QUEUE_SIZE_MAX, 256),
        ]);
        Self {
            values,
            features: 1 << 32,
            accepts_features: true,
            completes_reset: true,
            log: log.clone(),
        }
    }

    pub(crate) fn with_features(mut self, features: u64) -> Self {
        self.features = features;
        self
    }

    pub(crate) fn rejecting_features(mut self) -> Self {
        self.accepts_features = false;
        self
    }

    pub(crate) fn holding_reset(mut self) -> Self {
        self.completes_reset = false;
        self
    }

    pub(crate) fn set(&mut self, offset: usize, value: u32) {
        self.values.insert(offset, value);
    }
}

impl Registers for FakeRegisters {
    fn read(&self, offset: usize) -> u32 {
        self.log.push(Event::Read(offset));
        if offset == register::DEVICE_FEATURES {
            let select = self
                .values
                .get(&register::DEVICE_FEATURES_SELECT)
                .copied()
                .unwrap_or(0);
            let half = (self.features >> (32 * select)) & u64::from(u32::MAX);
            return u32::try_from(half).unwrap();
        }
        self.values.get(&offset).copied().unwrap_or(0)
    }

    fn write(&mut self, offset: usize, value: u32) {
        self.log.push(Event::Write(offset, value));
        let stored = match offset {
            register::STATUS if value == 0 && !self.completes_reset => status::ACKNOWLEDGE,
            register::STATUS if !self.accepts_features => value & !status::FEATURES_OK,
            _ => value,
        };
        self.values.insert(offset, stored);
    }

    fn barrier(&mut self) {
        self.log.push(Event::Barrier);
    }
}

impl FakeMemory {
    pub(crate) fn new(length: usize, log: &Log) -> Self {
        Self {
            bytes: vec![0xa5; length],
            address: REGION_ADDRESS,
            log: log.clone(),
        }
    }

    pub(crate) fn at(mut self, address: u64) -> Self {
        self.address = address;
        self
    }

    pub(crate) fn put_from_device(&mut self, offset: usize, source: &[u8]) {
        self.bytes[offset..offset + source.len()].copy_from_slice(source);
    }

    pub(crate) fn get(&self, offset: usize, length: usize) -> &[u8] {
        &self.bytes[offset..offset + length]
    }
}

impl DeviceMemory for FakeMemory {
    fn read(&self, offset: usize, destination: &mut [u8]) -> Result<(), OutOfRange> {
        self.log.push(Event::MemoryRead(offset));
        let source = self
            .bytes
            .get(offset..offset + destination.len())
            .ok_or(OutOfRange)?;
        destination.copy_from_slice(source);
        Ok(())
    }

    fn write(&mut self, offset: usize, source: &[u8]) -> Result<(), OutOfRange> {
        self.log.push(Event::MemoryWrite(offset, source.to_vec()));
        let target = self
            .bytes
            .get_mut(offset..offset + source.len())
            .ok_or(OutOfRange)?;
        target.copy_from_slice(source);
        Ok(())
    }

    fn physical_address(&self) -> u64 {
        self.address
    }
}
