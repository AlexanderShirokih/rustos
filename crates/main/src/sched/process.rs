use alloc::{sync::Arc, vec::Vec};
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

/// Слот-таблица процессов фиксированной ёмкости.
///
/// `next_id` гарантирует уникальность `ProcessId` даже при удалении
/// процессов из таблицы.
pub struct ProcessTable {
    slots: Vec<Option<Process>>,
    max_processes: usize,
    next_id: u32,
}

/// Ошибки операций с таблицей процессов.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ProcessTableError {
    /// Все слоты заняты.
    Full,
    /// Исчерпан числовой ID-пространство `NonZeroU32`.
    OutOfIds,
}

impl ProcessTable {
    pub fn new(max_processes: usize) -> Self {
        Self {
            slots: Vec::new(),
            max_processes,
            next_id: 1,
        }
    }

    pub fn insert(
        &mut self,
        name: &'static str,
        address_space: Arc<AddressSpace>,
    ) -> Result<ProcessId, ProcessTableError> {
        let raw = NonZeroU32::new(self.next_id).ok_or(ProcessTableError::OutOfIds)?;
        let id = ProcessId::new(raw);
        self.next_id = self
            .next_id
            .checked_add(1)
            .ok_or(ProcessTableError::OutOfIds)?;

        if let Some(index) = self.slots.iter().position(|slot| slot.is_none()) {
            self.slots[index] = Some(Process::new(id, name, address_space));
            return Ok(id);
        }

        if self.slots.len() >= self.max_processes {
            return Err(ProcessTableError::Full);
        }

        self.slots.push(Some(Process::new(id, name, address_space)));
        Ok(id)
    }

    pub fn get(&self, id: ProcessId) -> Option<&Process> {
        self.slots
            .iter()
            .filter_map(|s| s.as_ref())
            .find(|p| p.id() == id)
    }
}

impl Default for ProcessTable {
    fn default() -> Self {
        Self::new(64)
    }
}
