use core::cell::RefCell;
use std::{collections::HashMap, rc::Rc};

use crate::{
    access::{AccessWidth, DeviceMemory, OutOfRange, Registers},
    transport::{DeviceType, register, status},
};

pub(crate) const REGION_ADDRESS: u64 = 0x1_4000_0000;

const CONFIG_BYTES: usize = 0x100;

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum Event {
    Read(usize),
    Write(usize, u32),
    ConfigRead(usize, AccessWidth),
    ConfigWrite(usize, AccessWidth, u32),
    Barrier,
    MemoryRead(usize),
    MemoryWrite(usize, Vec<u8>),
}

#[derive(Clone, Default)]
pub(crate) struct Log(Rc<RefCell<Vec<Event>>>);

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum RegisterFailure {
    Read(usize),
    Write(usize),
}

pub(crate) struct FakeRegisters {
    read_refused: Option<usize>,
    write_refused: Option<usize>,
    values: HashMap<usize, u32>,
    config: [u8; CONFIG_BYTES],
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
        Self::new(DeviceType::Gpu, log)
    }

    pub(crate) fn new(device: DeviceType, log: &Log) -> Self {
        let values = HashMap::from([
            (register::MAGIC_VALUE, 0x7472_6976),
            (register::VERSION, 2),
            (register::DEVICE_ID, device as u32),
            (register::QUEUE_SIZE_MAX, 256),
        ]);
        Self {
            read_refused: None,
            write_refused: None,
            values,
            config: [0; CONFIG_BYTES],
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

    pub(crate) fn refusing_read(mut self, offset: usize) -> Self {
        self.read_refused = Some(offset);
        self
    }

    pub(crate) fn refusing_write(mut self, offset: usize) -> Self {
        self.write_refused = Some(offset);
        self
    }

    pub(crate) fn set(&mut self, offset: usize, value: u32) {
        self.values.insert(offset, value);
    }

    pub(crate) fn set_config(&mut self, offset: usize, bytes: &[u8]) {
        self.config[offset..offset + bytes.len()].copy_from_slice(bytes);
    }
}

#[allow(clippy::unused_async_trait_impl)]
impl Registers for FakeRegisters {
    type Error = RegisterFailure;

    async fn read(&self, offset: usize) -> Result<u32, RegisterFailure> {
        self.log.push(Event::Read(offset));
        if self.read_refused == Some(offset) {
            return Err(RegisterFailure::Read(offset));
        }

        if offset == register::DEVICE_FEATURES {
            let select = self
                .values
                .get(&register::DEVICE_FEATURES_SELECT)
                .copied()
                .unwrap_or(0);
            let half = (self.features >> (32 * select)) & u64::from(u32::MAX);
            return u32::try_from(half).map_err(|_| RegisterFailure::Read(offset));
        }

        Ok(self.values.get(&offset).copied().unwrap_or(0))
    }

    async fn write(&mut self, offset: usize, value: u32) -> Result<(), RegisterFailure> {
        self.log.push(Event::Write(offset, value));
        if self.write_refused == Some(offset) {
            return Err(RegisterFailure::Write(offset));
        }

        let stored = match offset {
            register::STATUS if value == 0 && !self.completes_reset => status::ACKNOWLEDGE,
            register::STATUS if !self.accepts_features => value & !status::FEATURES_OK,
            _ => value,
        };

        self.values.insert(offset, stored);
        Ok(())
    }

    async fn read_config(&self, offset: usize, width: AccessWidth) -> Result<u32, RegisterFailure> {
        self.log.push(Event::ConfigRead(offset, width));
        let mut value = [0; 4];
        let field = self
            .config
            .get(offset..offset + bytes(width))
            .ok_or(RegisterFailure::Read(offset))?;
        value[..field.len()].copy_from_slice(field);
        Ok(u32::from_le_bytes(value))
    }

    async fn write_config(
        &mut self,
        offset: usize,
        width: AccessWidth,
        value: u32,
    ) -> Result<(), RegisterFailure> {
        self.log.push(Event::ConfigWrite(offset, width, value));
        let field = self
            .config
            .get_mut(offset..offset + bytes(width))
            .ok_or(RegisterFailure::Write(offset))?;
        field.copy_from_slice(&value.to_le_bytes()[..bytes(width)]);
        Ok(())
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

fn bytes(width: AccessWidth) -> usize {
    match width {
        AccessWidth::Byte => 1,
        AccessWidth::Half => 2,
        AccessWidth::Word => 4,
    }
}

pub(crate) fn complete<F: core::future::Future>(future: F) -> F::Output {
    let mut future = core::pin::pin!(future);
    match future.as_mut().poll(&mut core::task::Context::from_waker(
        core::task::Waker::noop(),
    )) {
        core::task::Poll::Ready(output) => output,
        core::task::Poll::Pending => panic!("register model waited for an external event"),
    }
}
