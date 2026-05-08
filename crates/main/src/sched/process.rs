use alloc::{sync::Arc, vec::Vec};
use core::{
    num::NonZeroU32,
    sync::atomic::{AtomicUsize, Ordering},
};

use collections::MutexCell;
use drivers_common::services::scheduler::ProcessId;
use memory::user_vm_allocator::UserVmAllocator;

use super::address_space::AddressSpace;
use crate::kobject::HandleTable;

pub struct Process {
    id: ProcessId,
    name: &'static str,
    address_space: Arc<AddressSpace>,
    handle_table: Arc<MutexCell<HandleTable>>,
    /// Per-process аллокатор user-VA. `None` для kernel-процессов (idle,
    /// kernel-thread'ы) - у них пользовательской памяти нет, syscall'ы
    /// `vm_*` для них вернут `WrongType`.
    user_vm: Option<Arc<MutexCell<UserVmAllocator>>>,
    /// Количество живых thread'ов, привязанных к этому процессу. Декремент
    /// при `thread_exit`; ноль - сигнал scheduler-у удалить процесс.
    thread_count: AtomicUsize,
}

impl Process {
    pub fn new(id: ProcessId, name: &'static str, address_space: Arc<AddressSpace>) -> Self {
        Self {
            id,
            name,
            address_space,
            handle_table: Arc::new(MutexCell::new(HandleTable::new())),
            user_vm: None,
            thread_count: AtomicUsize::new(1),
        }
    }

    /// Прикрепляет per-process аллокатор user-VA. Используется при создании
    /// user-процесса: scheduler инициализирует аллокатор от верхней границы
    /// загруженного образа до начала user-стека.
    pub fn with_user_vm(mut self, vm: UserVmAllocator) -> Self {
        self.user_vm = Some(Arc::new(MutexCell::new(vm)));
        self
    }

    /// `Arc` per-process аллокатора user-VA. Клонируется как `Arc`,
    /// чтобы syscall-handler-ы могли работать с аллокатором вне scheduler-lock.
    /// `None` - у процесса нет user-AS (kernel-процесс).
    pub fn user_vm(&self) -> Option<&Arc<MutexCell<UserVmAllocator>>> {
        self.user_vm.as_ref()
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

    /// Per-process таблица capability-handle'ов. Клонируется как `Arc`,
    /// чтобы IPC-функции могли работать с таблицей вне scheduler-lock.
    pub fn handle_table(&self) -> &Arc<MutexCell<HandleTable>> {
        &self.handle_table
    }

    /// Регистрирует ещё один thread, принадлежащий процессу.
    pub fn increment_thread_count(&self) {
        self.thread_count.fetch_add(1, Ordering::AcqRel);
    }

    /// Снимает с учёта thread; возвращает `true`, если это был последний и
    /// процесс готов к удалению.
    pub fn decrement_thread_count(&self) -> bool {
        // AcqRel: prior thread-state writes happen-before наблюдения нуля.
        let prev = self.thread_count.fetch_sub(1, Ordering::AcqRel);
        debug_assert!(prev > 0, "thread_count underflow on Process {:?}", self.id);
        prev == 1
    }

    pub fn thread_count(&self) -> usize {
        self.thread_count.load(Ordering::Acquire)
    }
}

impl core::fmt::Debug for Process {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        f.debug_struct("Process")
            .field("id", &self.id)
            .field("name", &self.name)
            .finish_non_exhaustive()
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
        self.insert_with(|id| Process::new(id, name, address_space))
    }

    /// Вариант [`Self::insert`] с фабрикой, получающей сгенерированный
    /// `ProcessId`. Используется, когда вызывающий хочет навесить на свежий
    /// `Process` дополнительное состояние (например, `with_user_vm`)
    /// перед регистрацией.
    pub fn insert_with<F>(&mut self, factory: F) -> Result<ProcessId, ProcessTableError>
    where
        F: FnOnce(ProcessId) -> Process,
    {
        let raw = NonZeroU32::new(self.next_id).ok_or(ProcessTableError::OutOfIds)?;
        let id = ProcessId::new(raw);
        self.next_id = self
            .next_id
            .checked_add(1)
            .ok_or(ProcessTableError::OutOfIds)?;

        let process = factory(id);
        debug_assert_eq!(process.id(), id, "factory must use the supplied id");

        if let Some(index) = self.slots.iter().position(Option::is_none) {
            self.slots[index] = Some(process);
            return Ok(id);
        }

        if self.slots.len() >= self.max_processes {
            return Err(ProcessTableError::Full);
        }

        self.slots.push(Some(process));
        Ok(id)
    }

    pub fn get(&self, id: ProcessId) -> Option<&Process> {
        self.slots
            .iter()
            .filter_map(|s| s.as_ref())
            .find(|p| p.id() == id)
    }

    /// Удаляет процесс из таблицы. Возвращает `Some(Process)` если найден.
    /// `Drop` владеемого `Process` дропает `Arc<AddressSpace>` -
    /// если это была последняя ссылка, AddressSpace разрушается, mapper
    /// возвращает все свои фреймы аллокатору.
    pub fn remove(&mut self, id: ProcessId) -> Option<Process> {
        for slot in &mut self.slots {
            if slot.as_ref().is_some_and(|p| p.id() == id) {
                return slot.take();
            }
        }
        None
    }

    /// Количество активных процессов.
    pub fn live_count(&self) -> usize {
        self.slots.iter().filter(|s| s.is_some()).count()
    }
}

impl Default for ProcessTable {
    fn default() -> Self {
        Self::new(64)
    }
}
