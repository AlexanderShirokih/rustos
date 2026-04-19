use alloc::sync::Arc;
use core::num::NonZeroU32;

use super::address_space::AddressSpace;

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
#[repr(transparent)]
pub struct ProcessId(NonZeroU32);

impl ProcessId {
    pub const fn new(raw: NonZeroU32) -> Self {
        Self(raw)
    }

    pub const fn raw(self) -> NonZeroU32 {
        self.0
    }
}

#[derive(Debug)]
pub struct Process {
    id: ProcessId,
    name: &'static str,
    address_space: Arc<AddressSpace>,
}

impl Process {
    pub fn new(id: ProcessId, name: &'static str, address_space: Arc<AddressSpace>) -> Self {
        Self {
            id,
            name,
            address_space,
        }
    }

    pub fn id(&self) -> ProcessId {
        self.id
    }

    pub fn name(&self) -> &'static str {
        self.name
    }

    pub fn address_space(&self) -> &Arc<AddressSpace> {
        &self.address_space
    }
}
