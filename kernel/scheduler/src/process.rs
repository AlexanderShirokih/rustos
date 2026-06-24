use alloc::{sync::Arc, vec::Vec};
use core::{
    num::NonZeroU32,
    sync::atomic::{AtomicUsize, Ordering},
};

use capability::{HandleTable, ProcessObject};
use collections::{LockCell, MutexCell};
use memory::{MemoryRegion, user_vm_allocator::UserVmAllocator};

use super::address_space::AddressSpace;
use crate::ProcessId;

const MAX_PROCESS_NAME_LEN: usize = 64;

/// Возвращается из [`Process::set_user_vm`], если образ уже загружен.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ProcessAlreadyLoaded;

pub struct Process {
    id: ProcessId,
    name: [u8; MAX_PROCESS_NAME_LEN],
    name_len: u8,
    address_space: Arc<AddressSpace>,
    handle_table: Arc<MutexCell<HandleTable>>,
    /// Per-process аллокатор user-VA. `None` для kernel-процессов (idle,
    /// kernel-thread'ы) - у них пользовательской памяти нет, syscall'ы
    /// `vm_*` для них вернут `WrongType`.
    user_vm: Option<Arc<MutexCell<UserVmAllocator>>>,
    /// Регионы загруженного образа. Объявлены после `address_space`/`user_vm`,
    /// чтобы PTE снимались до возврата фреймов в `FrameAllocator`.
    image_segments: Vec<Arc<MemoryRegion>>,
    /// Количество живых thread'ов, привязанных к этому процессу. Декремент
    /// при `thread_exit`; ноль - сигнал планировщику удалить процесс.
    thread_count: AtomicUsize,
    /// Lifecycle-capability target процесса: переживает запись в `ProcessTable`, чтобы
    /// держатели `Capability` могли наблюдать завершение процесса и читать
    /// `exit_code` после удаления процесса (zombie-семантика).
    target: Arc<ProcessObject>,
}

impl Process {
    pub fn new(id: ProcessId, name: &str, address_space: Arc<AddressSpace>) -> Self {
        Self {
            id,
            name: encode_name(name),
            name_len: name
                .len()
                .try_into()
                .expect("process name length is bounded by MAX_PROCESS_NAME_LEN"),
            address_space,
            handle_table: Arc::new(MutexCell::new(HandleTable::new())),
            user_vm: None,
            image_segments: Vec::new(),
            thread_count: AtomicUsize::new(1),
            target: ProcessObject::new(),
        }
    }

    /// Создаёт процесс с нулевым счётчиком потоков.
    pub fn empty(id: ProcessId, name: &str, address_space: Arc<AddressSpace>) -> Self {
        Self {
            id,
            name: encode_name(name),
            name_len: name
                .len()
                .try_into()
                .expect("process name length is bounded by MAX_PROCESS_NAME_LEN"),
            address_space,
            handle_table: Arc::new(MutexCell::new(HandleTable::new())),
            user_vm: None,
            image_segments: Vec::new(),
            thread_count: AtomicUsize::new(0),
            target: ProcessObject::new(),
        }
    }

    /// Прикрепляет per-process аллокатор user-VA (builder-стиль).
    pub fn with_user_vm(mut self, vm: UserVmAllocator) -> Self {
        self.user_vm = Some(Arc::new(MutexCell::new(vm)));
        self
    }

    /// In-place аналог [`Self::with_user_vm`]; отказывается перезаписать уже прикреплённый аллокатор.
    pub fn set_user_vm(&mut self, vm: UserVmAllocator) -> Result<(), ProcessAlreadyLoaded> {
        if self.user_vm.is_some() {
            return Err(ProcessAlreadyLoaded);
        }
        self.user_vm = Some(Arc::new(MutexCell::new(vm)));
        Ok(())
    }

    pub fn set_image_segments(&mut self, segments: Vec<Arc<MemoryRegion>>) {
        self.image_segments = segments;
    }

    /// `true`, если образ загружен (user-vm прикреплена).
    pub fn is_image_loaded(&self) -> bool {
        self.user_vm.is_some()
    }

    /// `true`, если в handle-таблице процесса нет живых handle'ов.
    pub fn handle_table_is_empty(&self) -> bool {
        self.handle_table.with_lock(|t| t.live_count() == 0)
    }

    /// `Arc` per-process аллокатора user-VA; `None` для kernel-процессов.
    pub fn user_vm(&self) -> Option<&Arc<MutexCell<UserVmAllocator>>> {
        self.user_vm.as_ref()
    }

    pub fn id(&self) -> ProcessId {
        self.id
    }

    pub fn name(&self) -> &str {
        let len = usize::from(self.name_len);
        core::str::from_utf8(&self.name[..len]).expect("process names are copied from valid UTF-8")
    }

    pub fn address_space(&self) -> &Arc<AddressSpace> {
        &self.address_space
    }

    /// Per-process таблица capability-handle'ов.
    pub fn handle_table(&self) -> &Arc<MutexCell<HandleTable>> {
        &self.handle_table
    }

    /// Lifecycle-capability target процесса; переживает удаление из `ProcessTable` (zombie-семантика).
    pub fn process_object(&self) -> &Arc<ProcessObject> {
        &self.target
    }

    /// Регистрирует ещё один поток, принадлежащий процессу.
    pub fn increment_thread_count(&self) {
        self.thread_count.fetch_add(1, Ordering::AcqRel);
    }

    /// Снимает с учёта поток; возвращает `true`, если это был последний.
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
        name: &str,
        address_space: Arc<AddressSpace>,
    ) -> Result<ProcessId, ProcessTableError> {
        self.insert_with(|id| Process::new(id, name, address_space))
    }

    /// Вариант [`Self::insert`] с фабрикой, получающей выделенный `ProcessId`.
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

    pub fn get_mut(&mut self, id: ProcessId) -> Option<&mut Process> {
        self.slots
            .iter_mut()
            .filter_map(|s| s.as_mut())
            .find(|p| p.id() == id)
    }

    /// Линейный обход живых процессов.
    pub fn iter(&self) -> impl Iterator<Item = &Process> {
        self.slots.iter().filter_map(|s| s.as_ref())
    }

    /// `&mut`-вариант [`Self::iter`] для in-place изменений.
    pub fn iter_mut_internal(&mut self) -> impl Iterator<Item = &mut Process> {
        self.slots.iter_mut().filter_map(|s| s.as_mut())
    }

    /// Удаляет процесс из таблицы; возвращает `Some(Process)` если найден.
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

fn encode_name(name: &str) -> [u8; MAX_PROCESS_NAME_LEN] {
    assert!(
        name.len() <= MAX_PROCESS_NAME_LEN,
        "process name is longer than MAX_PROCESS_NAME_LEN"
    );
    let mut buf = [0u8; MAX_PROCESS_NAME_LEN];
    buf[..name.len()].copy_from_slice(name.as_bytes());
    buf
}

#[cfg(test)]
mod tests {
    use core::num::NonZeroU32;

    use memory::{
        UserVmAllocator,
        virtual_address::{PageAlignedVirtualAddress, VirtualAddress},
    };

    use super::*;

    fn pid(raw: u32) -> ProcessId {
        ProcessId::new(NonZeroU32::new(raw).expect("non-zero pid"))
    }

    fn fresh_process() -> Process {
        Process::empty(pid(1), "p", AddressSpace::kernel())
    }

    fn make_vm() -> UserVmAllocator {
        UserVmAllocator::new(
            PageAlignedVirtualAddress::from_usize(0x4000_0000).expect("aligned"),
            VirtualAddress::new(0x4001_0000),
        )
    }

    #[test]
    fn is_image_loaded_false_for_fresh_process() {
        let p = fresh_process();
        assert!(!p.is_image_loaded());
    }

    #[test]
    fn set_user_vm_succeeds_on_fresh_process_and_marks_loaded() {
        let mut p = fresh_process();
        assert_eq!(p.set_user_vm(make_vm()), Ok(()));
        assert!(p.is_image_loaded());
    }

    #[test]
    fn set_user_vm_rejects_when_already_loaded() {
        let mut p = fresh_process();
        p.set_user_vm(make_vm()).expect("first call ok");
        assert_eq!(p.set_user_vm(make_vm()), Err(ProcessAlreadyLoaded));
    }

    #[test]
    fn handle_table_is_empty_for_fresh_process() {
        let p = fresh_process();
        assert!(p.handle_table_is_empty());
    }

    #[test]
    fn process_keeps_owned_name() {
        let p = Process::empty(pid(1), "user-proc", AddressSpace::kernel());
        assert_eq!(p.name(), "user-proc");
    }

    #[test]
    fn process_table_full_when_all_slots_occupied() {
        let mut table = ProcessTable::new(2);
        assert!(table.insert("a", AddressSpace::kernel()).is_ok());
        assert!(table.insert("b", AddressSpace::kernel()).is_ok());
        assert_eq!(
            table.insert("c", AddressSpace::kernel()),
            Err(ProcessTableError::Full)
        );
        assert_eq!(table.live_count(), 2);
    }

    #[test]
    fn process_table_reuses_slot_after_remove() {
        let mut table = ProcessTable::new(1);
        let first = table.insert("a", AddressSpace::kernel()).expect("first");
        assert_eq!(
            table.insert("b", AddressSpace::kernel()),
            Err(ProcessTableError::Full)
        );

        let removed = table.remove(first).expect("first present");
        assert_eq!(removed.id(), first);
        assert!(table.get(first).is_none());

        let second = table.insert("b", AddressSpace::kernel()).expect("reuse");
        assert_ne!(first, second, "reused slot must still yield a fresh id");
        assert_eq!(table.live_count(), 1);
    }

    #[test]
    fn process_table_out_of_ids_when_id_space_exhausted() {
        let mut table = ProcessTable::new(4);
        table.next_id = u32::MAX;
        assert_eq!(
            table.insert("overflow", AddressSpace::kernel()),
            Err(ProcessTableError::OutOfIds)
        );
        assert_eq!(table.live_count(), 0);
    }
}
