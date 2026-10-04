use alloc::{sync::Arc, vec::Vec};
use core::{
    mem::offset_of,
    num::NonZeroU32,
    sync::atomic::{AtomicUsize, Ordering},
};

use capability::{HandleTable, ProcessObject};
use kbase::{LockCell, MutexCell, StaticString};
use memory::{MemoryRegion, UserVmContext, user_vm_allocator::UserVmAllocator};
use syscall::{MAX_PROCESS_NAME_LEN, ProcessName};

use super::address_space::AddressSpace;
use crate::ProcessId;

/// Возвращается из [`Process::set_user_vm`], если образ уже загружен.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ProcessAlreadyLoaded;

#[repr(C)]
pub struct Process {
    id: ProcessId,
    name: StaticString<MAX_PROCESS_NAME_LEN>,
    address_space: Arc<AddressSpace>,
    handle_table: Arc<MutexCell<HandleTable>>,
    user_vm: Option<Arc<MutexCell<UserVmAllocator>>>,
    image_segments: Vec<Arc<MemoryRegion>>,
    thread_count: AtomicUsize,
    target: Arc<ProcessObject>,
}

const _: () = assert!(
    offset_of!(Process, address_space) < offset_of!(Process, image_segments)
        && offset_of!(Process, user_vm) < offset_of!(Process, image_segments),
    "image segments drop after the address space and the user-VA allocator, so that the leaf \
     PTEs of the image are gone before its frames return to the frame allocator; `repr(C)` \
     makes the declaration order that Drop follows visible to this assert"
);

impl Process {
    pub fn new(id: ProcessId, name: ProcessName<'_>, address_space: Arc<AddressSpace>) -> Self {
        Self::with_thread_count(id, name, address_space, 1)
    }

    /// Создаёт процесс с нулевым счётчиком потоков.
    pub fn empty(id: ProcessId, name: ProcessName<'_>, address_space: Arc<AddressSpace>) -> Self {
        Self::with_thread_count(id, name, address_space, 0)
    }

    fn with_thread_count(
        id: ProcessId,
        name: ProcessName<'_>,
        address_space: Arc<AddressSpace>,
        thread_count: usize,
    ) -> Self {
        Self {
            id,
            name: StaticString::truncating(name.as_str()),
            address_space,
            handle_table: Arc::new(MutexCell::new(HandleTable::new())),
            user_vm: None,
            image_segments: Vec::new(),
            thread_count: AtomicUsize::new(thread_count),
            target: ProcessObject::new(),
        }
    }

    /// Прикрепляет per-process аллокатор user-VA (builder-стиль).
    pub fn with_user_vm(mut self, vm: UserVmAllocator) -> Self {
        self.user_vm = Some(Arc::new(MutexCell::new(vm)));
        self
    }

    /// In-place аналог [`Self::with_user_vm`]. Отказывается перезаписать
    /// уже прикреплённый аллокатор.
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

    /// `true`, если в handle-таблице процесса нет живых handle.
    pub fn handle_table_is_empty(&self) -> bool {
        self.handle_table.with_lock(|t| t.live_count() == 0)
    }

    /// `Arc` per-process аллокатора user-VA. `None` до загрузки образа и для
    /// kernel-процессов.
    pub fn user_vm(&self) -> Option<&Arc<MutexCell<UserVmAllocator>>> {
        self.user_vm.as_ref()
    }

    pub fn id(&self) -> ProcessId {
        self.id
    }

    pub fn name(&self) -> &str {
        self.name.as_str()
    }

    pub fn address_space(&self) -> &Arc<AddressSpace> {
        &self.address_space
    }

    /// Per-process таблица capability-handle.
    pub fn handle_table(&self) -> &Arc<MutexCell<HandleTable>> {
        &self.handle_table
    }

    /// Lifecycle-capability target процесса. Переживает удаление из
    /// `ProcessTable` (zombie-семантика).
    pub fn process_object(&self) -> &Arc<ProcessObject> {
        &self.target
    }

    /// Регистрирует ещё один поток, принадлежащий процессу.
    pub fn increment_thread_count(&self) {
        self.thread_count.fetch_add(1, Ordering::AcqRel);
    }

    /// Снимает с учёта поток. Возвращает `true`, если это был последний.
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
            .field("name", &self.name.as_str())
            .finish_non_exhaustive()
    }
}

impl Drop for Process {
    fn drop(&mut self) {
        if let (Some(mapper), Some(allocator)) =
            (self.address_space.mapper_arc(), self.user_vm.clone())
        {
            UserVmContext::new(mapper, allocator).drain();
        }
        self.target.clear_metering();
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
        name: ProcessName<'_>,
        address_space: Arc<AddressSpace>,
    ) -> Result<ProcessId, ProcessTableError> {
        self.insert_with(|id| Process::new(id, name, address_space))
            .map(|process| process.id())
    }

    /// Вариант [`Self::insert`] с фабрикой, получающей выделенный `ProcessId`.
    ///
    /// Возвращает сам вставленный процесс. Его id доступен через [`Process::id`].
    pub fn insert_with<F>(&mut self, factory: F) -> Result<&mut Process, ProcessTableError>
    where
        F: FnOnce(ProcessId) -> Process,
    {
        let raw = NonZeroU32::new(self.next_id).ok_or(ProcessTableError::OutOfIds)?;
        let id = ProcessId::new(raw);
        self.next_id = self
            .next_id
            .checked_add(1)
            .ok_or(ProcessTableError::OutOfIds)?;

        // Слот выбирается до фабрики. На переполнении её побочных эффектов не будет.
        let index = if let Some(index) = self.slots.iter().position(Option::is_none) {
            index
        } else {
            if self.slots.len() >= self.max_processes {
                return Err(ProcessTableError::Full);
            }
            self.slots.push(None);
            self.slots.len() - 1
        };

        let process = factory(id);
        debug_assert_eq!(process.id(), id, "factory must use the supplied id");

        let slot = self.slots.get_mut(index).ok_or(ProcessTableError::Full)?;
        Ok(slot.insert(process))
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

    /// Удаляет процесс из таблицы. Возвращает `Some(Process)` если найден.
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

    fn name(value: &str) -> ProcessName<'_> {
        ProcessName::new(value).expect("valid process name")
    }

    fn fresh_process() -> Process {
        Process::empty(pid(1), name("p"), AddressSpace::kernel())
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
        let p = Process::empty(pid(1), name("user-proc"), AddressSpace::kernel());
        assert_eq!(p.name(), "user-proc");
    }

    #[test]
    fn process_table_full_when_all_slots_occupied() {
        let mut table = ProcessTable::new(2);
        assert!(table.insert(name("a"), AddressSpace::kernel()).is_ok());
        assert!(table.insert(name("b"), AddressSpace::kernel()).is_ok());
        assert_eq!(
            table.insert(name("c"), AddressSpace::kernel()),
            Err(ProcessTableError::Full)
        );
        assert_eq!(table.live_count(), 2);
    }

    #[test]
    fn process_table_reuses_slot_after_remove() {
        let mut table = ProcessTable::new(1);
        let first = table
            .insert(name("a"), AddressSpace::kernel())
            .expect("first");
        assert_eq!(
            table.insert(name("b"), AddressSpace::kernel()),
            Err(ProcessTableError::Full)
        );

        let removed = table.remove(first).expect("first present");
        assert_eq!(removed.id(), first);
        assert!(table.get(first).is_none());

        let second = table
            .insert(name("b"), AddressSpace::kernel())
            .expect("reuse");
        assert_ne!(first, second, "reused slot must still yield a fresh id");
        assert_eq!(table.live_count(), 1);
    }

    #[test]
    fn process_table_out_of_ids_when_id_space_exhausted() {
        let mut table = ProcessTable::new(4);
        table.next_id = u32::MAX;
        assert_eq!(
            table.insert(name("overflow"), AddressSpace::kernel()),
            Err(ProcessTableError::OutOfIds)
        );
        assert_eq!(table.live_count(), 0);
    }
}
