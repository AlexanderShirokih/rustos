//! Непрерывный регион памяти, доступный устройству.

use kernel_objects::{Mapping, MemoryRegion, Resource};
use runtime::{MemoryAccess, UserMemFlags};
use syscall::PAGE_SIZE;
use virtio::{DeviceMemory, OutOfRange};

const HALF_WORD: usize = 2;

pub(crate) struct DmaRegion {
    mapping: Mapping,
    address: u64,
    _region: MemoryRegion,
}

impl DmaRegion {
    pub(crate) fn new(size: u64) -> Result<Self, kernel_objects::Error> {
        let size = size
            .checked_next_multiple_of(PAGE_SIZE as u64)
            .ok_or(kernel_objects::Error::OutOfBounds)?;
        let region = Resource::self_resource()?.create_contiguous(size, MemoryAccess::RW)?;
        let address = region.inspect()?.base;
        let mapping = region.map(size, UserMemFlags::ReadWrite)?;
        Ok(Self {
            mapping,
            address,
            _region: region,
        })
    }

    pub(crate) fn copy_from(
        &mut self,
        source: &Mapping,
        offset: usize,
        len: usize,
    ) -> Result<(), kernel_objects::Error> {
        source.copy_to(offset, &self.mapping, offset, len)
    }
}

impl DeviceMemory for DmaRegion {
    fn read(&self, offset: usize, destination: &mut [u8]) -> Result<(), OutOfRange> {
        if let Ok(half_word) = <&mut [u8; HALF_WORD]>::try_from(&mut *destination)
            && offset.is_multiple_of(HALF_WORD)
        {
            let value = self.mapping.read_u16_at(offset).map_err(|_| OutOfRange)?;
            *half_word = value.to_ne_bytes();
            return Ok(());
        }
        self.mapping
            .copy_out_at(offset, destination)
            .map_err(|_| OutOfRange)
    }

    fn write(&mut self, offset: usize, source: &[u8]) -> Result<(), OutOfRange> {
        if let Ok(half_word) = <[u8; HALF_WORD]>::try_from(source)
            && offset.is_multiple_of(HALF_WORD)
        {
            let value = u16::from_ne_bytes(half_word);
            return self
                .mapping
                .write_u16_at(offset, value)
                .map_err(|_| OutOfRange);
        }
        self.mapping
            .copy_in_at(offset, source)
            .map_err(|_| OutOfRange)
    }

    fn physical_address(&self) -> u64 {
        self.address
    }
}
