use alloc::{sync::Arc, vec::Vec};

use memory::MemoryRegion;

use super::{
    errors::IpcError,
    handle::{Handle, HandleId},
    object::KObject,
    port::Port,
    process::ProcessObject,
    reply::Reply,
    resource::Resource,
    rev_node::{RevocationHook, revoke_subtree},
    rights::Rights,
    signal::Signal,
    thread::ThreadObject,
    wait::CancelTarget,
};

/// Per-process слот-таблица handle'ов.
pub struct HandleTable {
    slots: Vec<Slot>,
    free_head: Option<u32>,
    capacity: u32,
}

struct Slot {
    generation: u16,
    state: SlotState,
    waiters: Vec<Arc<dyn CancelTarget>>,
}

enum SlotState {
    /// Слот свободен, ссылка на следующий элемент free-list'а.
    Free { next_free: Option<u32> },
    /// Слот удерживается под `commit_reserved`/`release_reservation`.
    Reserved,
    /// Слот занят живым handle'ом.
    Occupied(Handle),
    /// Generation исчерпана; слот выведен из оборота навсегда.
    Retired,
}

/// Зарезервированный слот: закрывается `commit_reserved` или `release_reservation`.
/// Забытая резервация навсегда удерживает слот.
#[derive(Debug, Clone, Copy)]
pub struct HandleReservation {
    slot: u32,
    generation: u16,
}

impl HandleReservation {
    pub fn handle_id(self) -> HandleId {
        HandleId::pack(self.generation, self.slot)
    }
}

impl HandleTable {
    /// Стандартная ёмкость таблицы handle'ов одного процесса.
    pub const DEFAULT_CAPACITY: u32 = 1024;

    pub fn new() -> Self {
        Self::with_capacity(Self::DEFAULT_CAPACITY)
    }

    pub fn with_capacity(capacity: u32) -> Self {
        let capacity = capacity.min(HandleId::MAX_SLOT_INDEX + 1);
        Self {
            slots: Vec::new(),
            free_head: None,
            capacity,
        }
    }

    pub fn capacity(&self) -> u32 {
        self.capacity
    }

    /// Текущее число занятых слотов. Используется только в тестах.
    pub fn live_count(&self) -> usize {
        self.slots
            .iter()
            .filter(|s| matches!(s.state, SlotState::Occupied(_)))
            .count()
    }

    /// Регистрирует handle, возвращая стабильный идентификатор.
    /// На `OutOfHandles` объект закрывается.
    /// Если caller'у важно сохранить объект на ошибке, используется
    /// [`Self::try_insert`].
    pub fn insert(&mut self, handle: Handle) -> Result<HandleId, IpcError> {
        self.try_insert(handle).map_err(|(e, _)| e)
    }

    /// То же, что [`Self::insert`], но при `OutOfHandles` возвращает
    /// `Handle` обратно вместо его закрытия.
    pub fn try_insert(&mut self, handle: Handle) -> Result<HandleId, (IpcError, Handle)> {
        if let Some(idx) = self.pop_free_slot() {
            let slot = &mut self.slots[idx as usize];
            // pop_free_slot гарантирует, что generation ещё не исчерпана.
            slot.generation += 1;
            slot.state = SlotState::Occupied(handle);
            slot.waiters.clear();
            return Ok(HandleId::pack(slot.generation, idx));
        }

        if (self.slots.len() as u32) >= self.capacity {
            return Err((IpcError::OutOfHandles, handle));
        }

        let idx = self.slots.len() as u32;
        self.slots.push(Slot {
            generation: 1,
            state: SlotState::Occupied(handle),
            waiters: Vec::new(),
        });
        Ok(HandleId::pack(1, idx))
    }

    /// Резервирует слот; на исчерпании ёмкости - `OutOfHandles` без эффектов.
    pub fn reserve_slot(&mut self) -> Result<HandleReservation, IpcError> {
        if let Some(idx) = self.pop_free_slot() {
            let slot = &mut self.slots[idx as usize];
            slot.generation += 1;
            slot.state = SlotState::Reserved;
            slot.waiters.clear();
            return Ok(HandleReservation {
                slot: idx,
                generation: slot.generation,
            });
        }

        if (self.slots.len() as u32) >= self.capacity {
            return Err(IpcError::OutOfHandles);
        }

        let idx = self.slots.len() as u32;
        self.slots.push(Slot {
            generation: 1,
            state: SlotState::Reserved,
            waiters: Vec::new(),
        });
        Ok(HandleReservation {
            slot: idx,
            generation: 1,
        })
    }

    /// Вставляет `handle` в зарезервированный слот. Паника на use-after-release.
    pub fn commit_reserved(&mut self, reservation: HandleReservation, handle: Handle) -> HandleId {
        let slot = self
            .slots
            .get_mut(reservation.slot as usize)
            .expect("reservation slot must exist");
        assert!(
            matches!(slot.state, SlotState::Reserved) && slot.generation == reservation.generation,
            "reservation mismatch: slot not in Reserved state with expected generation",
        );
        slot.state = SlotState::Occupied(handle);
        HandleId::pack(slot.generation, reservation.slot)
    }

    /// Возвращает зарезервированный слот в free-list. Паника на use-after-release.
    pub fn release_reservation(&mut self, reservation: HandleReservation) {
        let slot = self
            .slots
            .get_mut(reservation.slot as usize)
            .expect("reservation slot must exist");
        assert!(
            matches!(slot.state, SlotState::Reserved) && slot.generation == reservation.generation,
            "release_reservation mismatch: slot not in Reserved state with expected generation",
        );
        slot.generation -= 1;
        let prev_head = self.free_head;
        slot.state = SlotState::Free {
            next_free: prev_head,
        };
        self.free_head = Some(reservation.slot);
    }

    /// Удаляет handle из таблицы. Будит каждый cancel-target с исходом
    /// [`IpcError::Canceled`](IpcError::Canceled).
    pub fn remove(&mut self, id: HandleId) -> Result<Handle, IpcError> {
        let handle = self.take_slot(id)?;
        revoke_subtree(handle.node());
        Ok(handle)
    }

    fn take_slot(&mut self, id: HandleId) -> Result<Handle, IpcError> {
        let idx = id.slot();
        let slot = self
            .slots
            .get_mut(idx as usize)
            .ok_or(IpcError::BadHandle)?;

        if slot.generation != id.generation() {
            return Err(IpcError::BadHandle);
        }
        if !matches!(slot.state, SlotState::Occupied(_)) {
            return Err(IpcError::BadHandle);
        }

        let prev_head = self.free_head;
        let taken = core::mem::replace(
            &mut slot.state,
            SlotState::Free {
                next_free: prev_head,
            },
        );
        let waiters = core::mem::take(&mut slot.waiters);
        self.free_head = Some(idx);

        for w in waiters {
            w.cancel();
        }

        match taken {
            SlotState::Occupied(handle) => Ok(handle),
            // Невозможно по проверке выше, но обходимся без unreachable!.
            _ => Err(IpcError::BadHandle),
        }
    }

    /// Регистрирует cancel-target на слот `id`. При закрытии/передаче
    /// handle target получит `cancel()`.
    pub fn register_cancel(
        &mut self,
        id: HandleId,
        target: Arc<dyn CancelTarget>,
    ) -> Result<(), IpcError> {
        let slot = self
            .slots
            .get_mut(id.slot() as usize)
            .ok_or(IpcError::BadHandle)?;
        if slot.generation != id.generation() {
            return Err(IpcError::BadHandle);
        }
        if !matches!(slot.state, SlotState::Occupied(_)) {
            return Err(IpcError::BadHandle);
        }
        slot.waiters.push(target);
        Ok(())
    }

    /// Регистрирует [`RevocationHook`] на узле деривации капы `id`. При
    /// отзыве капы (close/Drop любого её предка) hook получит `revoke()`.
    pub fn register_revocation_hook(
        &self,
        id: HandleId,
        hook: alloc::sync::Weak<dyn RevocationHook>,
    ) -> Result<(), IpcError> {
        let handle = self.lookup(id)?;
        handle.node().register_hook(hook);
        Ok(())
    }

    /// Снимает cancel-target по identity.
    pub fn unregister_cancel(&mut self, id: HandleId, target: &Arc<dyn CancelTarget>) {
        let Some(slot) = self.slots.get_mut(id.slot() as usize) else {
            return;
        };
        let target_ptr = Arc::as_ptr(target).cast::<()>();
        if let Some(pos) = slot
            .waiters
            .iter()
            .position(|w| Arc::as_ptr(w).cast::<()>() == target_ptr)
        {
            slot.waiters.swap_remove(pos);
        }
    }

    /// Проверка прав без проверки типа.
    pub fn get(&self, id: HandleId, need: Rights) -> Result<&Handle, IpcError> {
        let handle = self.lookup(id)?;
        if !handle.rights().contains(need) {
            return Err(IpcError::AccessDenied);
        }
        Ok(handle)
    }

    /// Извлекает `Arc<Port>` с проверкой прав и типа.
    pub fn get_port(&self, id: HandleId, need: Rights) -> Result<Arc<Port>, IpcError> {
        let h = self.lookup(id)?;
        if !h.rights().contains(need) {
            return Err(IpcError::AccessDenied);
        }
        match &h.object {
            KObject::Port(e) => Ok(e.clone()),
            KObject::Signal(_)
            | KObject::Process(_)
            | KObject::Thread(_)
            | KObject::Memory(_)
            | KObject::Resource(_)
            | KObject::Reply(_) => Err(IpcError::WrongType),
        }
    }

    /// Извлекает `Arc<Port>` вместе с badge хендла, с проверкой прав и типа.
    pub fn get_port_with_badge(
        &self,
        id: HandleId,
        need: Rights,
    ) -> Result<(Arc<Port>, u64), IpcError> {
        let h = self.lookup(id)?;
        if !h.rights().contains(need) {
            return Err(IpcError::AccessDenied);
        }
        match &h.object {
            KObject::Port(e) => Ok((e.clone(), h.badge())),
            KObject::Signal(_)
            | KObject::Process(_)
            | KObject::Thread(_)
            | KObject::Memory(_)
            | KObject::Resource(_)
            | KObject::Reply(_) => Err(IpcError::WrongType),
        }
    }

    /// Извлекает `Arc<Reply>` с проверкой прав и типа.
    pub fn get_reply(&self, id: HandleId, need: Rights) -> Result<Arc<Reply>, IpcError> {
        let h = self.lookup(id)?;
        if !h.rights().contains(need) {
            return Err(IpcError::AccessDenied);
        }
        match &h.object {
            KObject::Reply(r) => Ok(r.clone()),
            KObject::Signal(_)
            | KObject::Process(_)
            | KObject::Thread(_)
            | KObject::Memory(_)
            | KObject::Resource(_)
            | KObject::Port(_) => Err(IpcError::WrongType),
        }
    }

    /// Извлекает `Arc<Signal>` с проверкой прав и типа.
    pub fn get_signal(&self, id: HandleId, need: Rights) -> Result<Arc<Signal>, IpcError> {
        let h = self.lookup(id)?;
        if !h.rights().contains(need) {
            return Err(IpcError::AccessDenied);
        }
        match &h.object {
            KObject::Signal(n) => Ok(n.clone()),
            KObject::Process(_)
            | KObject::Thread(_)
            | KObject::Memory(_)
            | KObject::Resource(_)
            | KObject::Port(_)
            | KObject::Reply(_) => Err(IpcError::WrongType),
        }
    }

    /// Извлекает `Arc<ProcessObject>` с проверкой прав и типа.
    pub fn get_process(&self, id: HandleId, need: Rights) -> Result<Arc<ProcessObject>, IpcError> {
        let h = self.lookup(id)?;
        if !h.rights().contains(need) {
            return Err(IpcError::AccessDenied);
        }
        match &h.object {
            KObject::Process(p) => Ok(p.clone()),
            KObject::Signal(_)
            | KObject::Thread(_)
            | KObject::Memory(_)
            | KObject::Resource(_)
            | KObject::Port(_)
            | KObject::Reply(_) => Err(IpcError::WrongType),
        }
    }

    /// Извлекает `Arc<ThreadObject>` с проверкой прав и типа.
    pub fn get_thread(&self, id: HandleId, need: Rights) -> Result<Arc<ThreadObject>, IpcError> {
        let h = self.lookup(id)?;
        if !h.rights().contains(need) {
            return Err(IpcError::AccessDenied);
        }
        match &h.object {
            KObject::Thread(t) => Ok(t.clone()),
            KObject::Signal(_)
            | KObject::Process(_)
            | KObject::Memory(_)
            | KObject::Resource(_)
            | KObject::Port(_)
            | KObject::Reply(_) => Err(IpcError::WrongType),
        }
    }

    /// Извлекает `Arc<MemoryRegion>` с проверкой прав и типа.
    pub fn get_memory(&self, id: HandleId, need: Rights) -> Result<Arc<MemoryRegion>, IpcError> {
        self.get_memory_with_rights(id, need).map(|(m, _)| m)
    }

    /// Извлекает `Arc<MemoryRegion>` вместе с полным набором `Rights` handle'а.
    pub fn get_memory_with_rights(
        &self,
        id: HandleId,
        need: Rights,
    ) -> Result<(Arc<MemoryRegion>, Rights), IpcError> {
        let h = self.lookup(id)?;
        if !h.rights().contains(need) {
            return Err(IpcError::AccessDenied);
        }
        match &h.object {
            KObject::Memory(m) => Ok((m.clone(), h.rights())),
            KObject::Signal(_)
            | KObject::Process(_)
            | KObject::Thread(_)
            | KObject::Resource(_)
            | KObject::Port(_)
            | KObject::Reply(_) => Err(IpcError::WrongType),
        }
    }

    /// Извлекает `Arc<Resource>` с проверкой прав и типа.
    pub fn get_resource(&self, id: HandleId, need: Rights) -> Result<Arc<Resource>, IpcError> {
        let h = self.lookup(id)?;
        if !h.rights().contains(need) {
            return Err(IpcError::AccessDenied);
        }
        match &h.object {
            KObject::Resource(r) => Ok(r.clone()),
            KObject::Signal(_)
            | KObject::Process(_)
            | KObject::Thread(_)
            | KObject::Memory(_)
            | KObject::Port(_)
            | KObject::Reply(_) => Err(IpcError::WrongType),
        }
    }

    /// Доступ к KO без проверки конкретного типа - для wait-пути,
    /// который применим к любому signalable (Signal/Process/...).
    pub fn clone_object(&self, id: HandleId, need: Rights) -> Result<KObject, IpcError> {
        let handle = self.lookup(id)?;
        if !handle.rights().contains(need) {
            return Err(IpcError::AccessDenied);
        }
        Ok(handle.object().clone())
    }

    /// Атомарно проверяет, что все `ids` существуют, имеют `min_rights`
    /// и нет дубликатов; затем удаляет их и возвращает `Vec<Handle>`.
    pub fn try_drain_for_transfer(
        &mut self,
        ids: &[HandleId],
        min_rights: Rights,
    ) -> Result<Vec<Handle>, IpcError> {
        for (i, id) in ids.iter().enumerate() {
            if ids[..i].iter().any(|prev| prev == id) {
                return Err(IpcError::BadHandle);
            }
        }
        for id in ids {
            self.get(*id, min_rights)?;
        }
        let mut drained = Vec::with_capacity(ids.len());
        for id in ids {
            // Валидация выше гарантирует, что take_slot не упадёт: id найден,
            // generation совпадает, rights включают min_rights.
            let h = self.take_slot(*id).expect("validated above");
            drained.push(h);
        }
        Ok(drained)
    }

    /// Создаёт новый handle на тот же KO с подмножеством прав и (опционально) badge.
    /// Семантика значка - set-once, см. [`Handle::duplicate`].
    pub fn duplicate(
        &mut self,
        id: HandleId,
        new_rights: Rights,
        new_badge: u64,
    ) -> Result<HandleId, IpcError> {
        let dup = {
            let handle = self.lookup(id)?;
            handle.duplicate(new_rights, new_badge)?
        };
        self.insert(dup)
    }

    fn lookup(&self, id: HandleId) -> Result<&Handle, IpcError> {
        let idx = id.slot();
        let slot = self.slots.get(idx as usize).ok_or(IpcError::BadHandle)?;
        if slot.generation != id.generation() {
            return Err(IpcError::BadHandle);
        }
        match &slot.state {
            SlotState::Occupied(handle) => {
                if handle.node().is_alive() {
                    Ok(handle)
                } else {
                    Err(IpcError::Revoked)
                }
            }
            SlotState::Free { .. } | SlotState::Reserved | SlotState::Retired => {
                Err(IpcError::BadHandle)
            }
        }
    }

    fn pop_free_slot(&mut self) -> Option<u32> {
        while let Some(idx) = self.free_head {
            let slot = &mut self.slots[idx as usize];

            let SlotState::Free { next_free } = slot.state else {
                self.free_head = None;
                return None;
            };
            self.free_head = next_free;
            if slot.generation == HandleId::MAX_GENERATION {
                slot.state = SlotState::Retired;
                continue;
            }
            return Some(idx);
        }
        None
    }
}

impl Default for HandleTable {
    fn default() -> Self {
        Self::new()
    }
}

impl Drop for HandleTable {
    /// Смерть таблицы (process exit) отзывает все её гранты.
    fn drop(&mut self) {
        for slot in &mut self.slots {
            for w in core::mem::take(&mut slot.waiters) {
                w.cancel();
            }
            if let SlotState::Occupied(handle) = &slot.state {
                revoke_subtree(handle.node());
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use core::sync::atomic::{AtomicUsize, Ordering};

    use super::{
        super::{
            object::KObject, port::Port, process::ProcessObject, rights::Rights, signal::Signal,
            thread::ThreadObject, wait::CancelTarget,
        },
        *,
    };

    struct CountingCancel(AtomicUsize);

    impl CountingCancel {
        fn new() -> Arc<Self> {
            Arc::new(Self(AtomicUsize::new(0)))
        }

        fn count(&self) -> usize {
            self.0.load(Ordering::Acquire)
        }
    }

    impl CancelTarget for CountingCancel {
        fn cancel(&self) {
            self.0.fetch_add(1, Ordering::AcqRel);
        }
    }

    fn make_handle(obj: KObject, rights: Rights) -> Handle {
        Handle::new(obj, rights)
    }

    fn signal_handle(rights: Rights) -> Handle {
        make_handle(KObject::Signal(Signal::new()), rights)
    }

    fn port_handle(rights: Rights) -> Handle {
        make_handle(KObject::Port(Port::new()), rights)
    }

    fn process_handle(rights: Rights) -> Handle {
        make_handle(KObject::Process(ProcessObject::new()), rights)
    }

    fn thread_handle(rights: Rights) -> Handle {
        make_handle(KObject::Thread(ThreadObject::new()), rights)
    }

    #[test]
    fn insert_and_get_round_trip() {
        let mut table = HandleTable::new();
        let h = port_handle(Rights::READ | Rights::WRITE);
        let koid = h.koid();

        let id = table.insert(h).unwrap();
        let got = table.get(id, Rights::READ).expect("get must succeed");
        assert_eq!(got.koid(), koid);
        assert_eq!(table.live_count(), 1);
    }

    #[test]
    fn get_with_missing_right_returns_access_denied() {
        let mut table = HandleTable::new();
        let id = table.insert(signal_handle(Rights::READ)).unwrap();

        assert_eq!(
            table.get(id, Rights::WRITE).unwrap_err(),
            IpcError::AccessDenied
        );
        assert!(table.get(id, Rights::READ).is_ok());
    }

    #[test]
    fn get_port_type_checks() {
        let mut table = HandleTable::new();
        let signal_id = table.insert(signal_handle(Rights::READ)).unwrap();
        let ep_id = table.insert(port_handle(Rights::READ)).unwrap();
        let proc_id = table.insert(process_handle(Rights::READ)).unwrap();
        let thread_id = table.insert(thread_handle(Rights::READ)).unwrap();

        assert!(table.get_signal(signal_id, Rights::READ).is_ok());
        assert!(table.get_port(ep_id, Rights::READ).is_ok());

        assert!(matches!(
            table.get_port(signal_id, Rights::READ),
            Err(IpcError::WrongType)
        ));
        assert!(matches!(
            table.get_signal(ep_id, Rights::READ),
            Err(IpcError::WrongType)
        ));
        assert!(matches!(
            table.get_port(proc_id, Rights::READ),
            Err(IpcError::WrongType)
        ));
        assert!(matches!(
            table.get_signal(thread_id, Rights::READ),
            Err(IpcError::WrongType)
        ));
    }

    #[test]
    fn get_process_type_checks() {
        let mut table = HandleTable::new();
        let proc_id = table.insert(process_handle(Rights::READ)).unwrap();
        let signal_id = table.insert(signal_handle(Rights::READ)).unwrap();
        let ep_id = table.insert(port_handle(Rights::READ)).unwrap();
        let thread_id = table.insert(thread_handle(Rights::READ)).unwrap();

        assert!(table.get_process(proc_id, Rights::READ).is_ok());
        assert!(matches!(
            table.get_process(signal_id, Rights::READ),
            Err(IpcError::WrongType)
        ));
        assert!(matches!(
            table.get_process(ep_id, Rights::READ),
            Err(IpcError::WrongType)
        ));
        assert!(matches!(
            table.get_process(thread_id, Rights::READ),
            Err(IpcError::WrongType)
        ));
    }

    #[test]
    fn get_thread_type_checks() {
        let mut table = HandleTable::new();
        let thread_id = table.insert(thread_handle(Rights::READ)).unwrap();
        let signal_id = table.insert(signal_handle(Rights::READ)).unwrap();
        let ep_id = table.insert(port_handle(Rights::READ)).unwrap();
        let proc_id = table.insert(process_handle(Rights::READ)).unwrap();

        assert!(table.get_thread(thread_id, Rights::READ).is_ok());
        assert!(matches!(
            table.get_thread(signal_id, Rights::READ),
            Err(IpcError::WrongType)
        ));
        assert!(matches!(
            table.get_thread(ep_id, Rights::READ),
            Err(IpcError::WrongType)
        ));
        assert!(matches!(
            table.get_thread(proc_id, Rights::READ),
            Err(IpcError::WrongType)
        ));
    }

    #[test]
    fn get_process_checks_rights() {
        let mut table = HandleTable::new();
        let id = table.insert(process_handle(Rights::READ)).unwrap();
        assert!(matches!(
            table.get_process(id, Rights::WRITE),
            Err(IpcError::AccessDenied)
        ));
        assert!(table.get_process(id, Rights::READ).is_ok());
    }

    #[test]
    fn get_thread_checks_rights() {
        let mut table = HandleTable::new();
        let id = table.insert(thread_handle(Rights::READ)).unwrap();
        assert!(matches!(
            table.get_thread(id, Rights::WRITE),
            Err(IpcError::AccessDenied)
        ));
        assert!(table.get_thread(id, Rights::READ).is_ok());
    }

    #[test]
    fn get_memory_type_checks() {
        use core::num::NonZeroUsize;

        use memory::{AccessMask, MemoryRegion};

        let region = Arc::new(MemoryRegion::create_physical(
            memory::physical_address::PageAlignedAddress::from_usize(0x4000_0000).unwrap(),
            NonZeroUsize::new(4096).unwrap(),
            AccessMask::R,
        ));
        let mut table = HandleTable::new();
        let mem_rights = Rights::WRITE | Rights::READ;
        let mem_id = table
            .insert(make_handle(KObject::Memory(region), mem_rights))
            .unwrap();
        let signal_id = table.insert(signal_handle(Rights::READ)).unwrap();
        let ep_id = table.insert(port_handle(Rights::READ)).unwrap();

        assert!(table.get_memory(mem_id, Rights::WRITE).is_ok());
        assert!(matches!(
            table.get_memory(signal_id, Rights::READ),
            Err(IpcError::WrongType)
        ));
        assert!(matches!(
            table.get_memory(ep_id, Rights::READ),
            Err(IpcError::WrongType)
        ));
        assert!(matches!(
            table.get_port(mem_id, Rights::READ),
            Err(IpcError::WrongType)
        ));
        assert!(matches!(
            table.get_signal(mem_id, Rights::READ),
            Err(IpcError::WrongType)
        ));
    }

    #[test]
    fn get_resource_type_checks() {
        use core::num::NonZeroUsize;

        use memory::{AccessMask, physical_address::PageAlignedAddress};

        use super::super::resource::Resource;

        let resource = Resource::new(
            PageAlignedAddress::from_usize(0x4000_0000).unwrap(),
            NonZeroUsize::new(4096).unwrap(),
            AccessMask::RW,
            0,
        );
        let mut table = HandleTable::new();
        let res_rights = Rights::WRITE | Rights::READ;
        let res_id = table
            .insert(make_handle(KObject::Resource(resource), res_rights))
            .unwrap();
        let signal_id = table.insert(signal_handle(Rights::READ)).unwrap();
        let ep_id = table.insert(port_handle(Rights::READ)).unwrap();

        assert!(table.get_resource(res_id, Rights::WRITE).is_ok());
        assert_eq!(
            table
                .get_resource(res_id, Rights::WRITE | Rights::EXECUTE)
                .unwrap_err(),
            IpcError::AccessDenied
        );
        assert!(matches!(
            table.get_resource(signal_id, Rights::READ),
            Err(IpcError::WrongType)
        ));
        assert!(matches!(
            table.get_resource(ep_id, Rights::READ),
            Err(IpcError::WrongType)
        ));
        assert!(matches!(
            table.get_port(res_id, Rights::READ),
            Err(IpcError::WrongType)
        ));
        assert!(matches!(
            table.get_signal(res_id, Rights::READ),
            Err(IpcError::WrongType)
        ));
    }

    #[test]
    fn remove_then_get_returns_bad_handle() {
        let mut table = HandleTable::new();
        let id = table.insert(port_handle(Rights::READ)).unwrap();

        let _ = table.remove(id).unwrap();
        assert_eq!(
            table.get(id, Rights::READ).unwrap_err(),
            IpcError::BadHandle
        );
    }

    #[test]
    fn double_close_returns_bad_handle() {
        let mut table = HandleTable::new();
        let id = table.insert(signal_handle(Rights::READ)).unwrap();

        table.remove(id).unwrap();
        assert_eq!(table.remove(id).unwrap_err(), IpcError::BadHandle);
    }

    #[test]
    fn out_of_range_slot_returns_bad_handle() {
        let table = HandleTable::with_capacity(4);
        let bogus = HandleId::pack(1, 999);
        assert_eq!(
            table.get(bogus, Rights::empty()).unwrap_err(),
            IpcError::BadHandle
        );
    }

    #[test]
    fn duplicate_subsets_rights() {
        let mut table = HandleTable::new();
        let rights = Rights::DUPLICATE | Rights::READ | Rights::WRITE;
        let id = table.insert(port_handle(rights)).unwrap();

        let dup = table.duplicate(id, Rights::READ, 0).unwrap();
        let got = table.get(dup, Rights::READ).unwrap();
        assert_eq!(got.rights(), Rights::READ);
        assert_eq!(
            table.get(dup, Rights::WRITE).unwrap_err(),
            IpcError::AccessDenied
        );
        assert!(table.get(id, Rights::WRITE).is_ok());
    }

    #[test]
    fn duplicate_rejects_extra_rights() {
        let mut table = HandleTable::new();
        let id = table
            .insert(port_handle(Rights::DUPLICATE | Rights::READ))
            .unwrap();

        assert_eq!(
            table
                .duplicate(id, Rights::READ | Rights::WRITE, 0)
                .unwrap_err(),
            IpcError::AccessDenied
        );
    }

    #[test]
    fn duplicate_requires_duplicate_right() {
        let mut table = HandleTable::new();
        let id = table.insert(port_handle(Rights::READ)).unwrap();

        assert_eq!(
            table.duplicate(id, Rights::READ, 0).unwrap_err(),
            IpcError::AccessDenied
        );
    }

    #[test]
    fn duplicate_badge_set_once_claims_unbadged() {
        let mut table = HandleTable::new();
        let rights = Rights::DUPLICATE | Rights::READ;
        let id = table.insert(port_handle(rights)).unwrap();

        let dup = table.duplicate(id, Rights::READ, 0xABCD).unwrap();
        assert_eq!(table.get(dup, Rights::READ).unwrap().badge(), 0xABCD);
        assert_eq!(table.get(id, Rights::READ).unwrap().badge(), 0);
    }

    #[test]
    fn duplicate_badge_inherits_when_zero() {
        let mut table = HandleTable::new();
        let rights = Rights::DUPLICATE | Rights::READ;
        let id = table.insert(port_handle(rights)).unwrap();
        let badged = table.duplicate(id, rights, 0x42).unwrap();

        let child = table.duplicate(badged, Rights::READ, 0).unwrap();
        assert_eq!(table.get(child, Rights::READ).unwrap().badge(), 0x42);
    }

    #[test]
    fn duplicate_badge_rebadge_rejected() {
        let mut table = HandleTable::new();
        let rights = Rights::DUPLICATE | Rights::READ;
        let id = table.insert(port_handle(rights)).unwrap();
        let badged = table.duplicate(id, rights, 0x11).unwrap();

        assert_eq!(
            table.duplicate(badged, Rights::READ, 0x22).unwrap_err(),
            IpcError::BadHandle
        );
    }

    #[test]
    fn slot_reuse_invalidates_old_id() {
        let mut table = HandleTable::with_capacity(2);
        let id1 = table.insert(signal_handle(Rights::READ)).unwrap();
        table.remove(id1).unwrap();

        let id2 = table.insert(signal_handle(Rights::READ)).unwrap();
        assert_eq!(id1.slot(), id2.slot());
        assert_ne!(id1.generation(), id2.generation());
        assert_eq!(
            table.get(id1, Rights::READ).unwrap_err(),
            IpcError::BadHandle
        );
        assert!(table.get(id2, Rights::READ).is_ok());
    }

    #[test]
    fn out_of_handles_when_capacity_reached() {
        let mut table = HandleTable::with_capacity(2);
        let _ = table.insert(signal_handle(Rights::READ)).unwrap();
        let _ = table.insert(signal_handle(Rights::READ)).unwrap();

        let err = table.insert(signal_handle(Rights::READ)).unwrap_err();
        assert_eq!(err, IpcError::OutOfHandles);
    }

    #[test]
    fn try_insert_returns_handle_on_out_of_handles() {
        use alloc::sync::Arc;

        let mut table = HandleTable::with_capacity(1);
        table.insert(signal_handle(Rights::READ)).unwrap();

        let signal = Signal::new();
        let weak = Arc::downgrade(&signal);
        let handle = Handle::new(KObject::Signal(signal), Rights::READ);
        let (err, returned) = table.try_insert(handle).unwrap_err();
        assert_eq!(err, IpcError::OutOfHandles);
        assert!(weak.upgrade().is_some());

        let mut other = HandleTable::with_capacity(1);
        other.insert(returned).unwrap();
        assert_eq!(other.live_count(), 1);
    }

    #[test]
    fn try_insert_success_matches_insert() {
        let mut table = HandleTable::with_capacity(2);
        let id = table.try_insert(signal_handle(Rights::READ)).unwrap();
        assert!(table.get(id, Rights::READ).is_ok());
    }

    #[test]
    fn generation_rollover_retires_slot() {
        let mut table = HandleTable::with_capacity(1);
        let max_gen = u32::from(HandleId::MAX_GENERATION);

        let mut id = table.insert(signal_handle(Rights::READ)).unwrap();
        assert_eq!(u32::from(id.generation()), 1);

        for expected_gen in 2..=max_gen {
            table.remove(id).unwrap();
            id = table.insert(signal_handle(Rights::READ)).unwrap();
            assert_eq!(u32::from(id.generation()), expected_gen);
        }

        table.remove(id).unwrap();
        assert_eq!(
            table.insert(signal_handle(Rights::READ)).unwrap_err(),
            IpcError::OutOfHandles
        );
    }

    #[test]
    fn try_drain_for_transfer_success() {
        let mut table = HandleTable::new();
        let id1 = table
            .insert(signal_handle(Rights::READ | Rights::TRANSFER))
            .unwrap();
        let id2 = table
            .insert(signal_handle(Rights::READ | Rights::TRANSFER))
            .unwrap();
        assert_eq!(table.live_count(), 2);

        let drained = table
            .try_drain_for_transfer(&[id1, id2], Rights::TRANSFER)
            .unwrap();
        assert_eq!(drained.len(), 2);
        assert_eq!(table.live_count(), 0);
    }

    #[test]
    fn try_drain_for_transfer_empty_ok() {
        let mut table = HandleTable::new();
        let drained = table.try_drain_for_transfer(&[], Rights::TRANSFER).unwrap();
        assert!(drained.is_empty());
        assert_eq!(table.live_count(), 0);
    }

    #[test]
    fn try_drain_for_transfer_missing_id_no_removal() {
        let mut table = HandleTable::new();
        let id = table
            .insert(signal_handle(Rights::READ | Rights::TRANSFER))
            .unwrap();
        let bogus = HandleId::pack(1, 999);

        let err = table
            .try_drain_for_transfer(&[id, bogus], Rights::TRANSFER)
            .unwrap_err();
        assert_eq!(err, IpcError::BadHandle);
        // Никаких изменений в таблице.
        assert_eq!(table.live_count(), 1);
        assert!(table.get(id, Rights::TRANSFER).is_ok());
    }

    #[test]
    fn try_drain_for_transfer_insufficient_rights_no_removal() {
        let mut table = HandleTable::new();
        let id = table.insert(signal_handle(Rights::READ)).unwrap();
        let err = table
            .try_drain_for_transfer(&[id], Rights::TRANSFER)
            .unwrap_err();
        assert_eq!(err, IpcError::AccessDenied);
        assert_eq!(table.live_count(), 1);
        assert!(table.get(id, Rights::READ).is_ok());
    }

    #[test]
    fn try_drain_for_transfer_duplicate_no_removal() {
        let mut table = HandleTable::new();
        let id = table
            .insert(signal_handle(Rights::READ | Rights::TRANSFER))
            .unwrap();
        let err = table
            .try_drain_for_transfer(&[id, id], Rights::TRANSFER)
            .unwrap_err();
        assert_eq!(err, IpcError::BadHandle);
        assert_eq!(table.live_count(), 1);
        assert!(table.get(id, Rights::TRANSFER).is_ok());
    }

    #[test]
    fn try_drain_for_transfer_partial_validation_atomic() {
        let mut table = HandleTable::new();
        let id1 = table
            .insert(signal_handle(Rights::READ | Rights::TRANSFER))
            .unwrap();
        let id2 = table
            .insert(signal_handle(Rights::READ | Rights::TRANSFER))
            .unwrap();
        let bogus = HandleId::pack(1, 999);

        let err = table
            .try_drain_for_transfer(&[id1, id2, bogus], Rights::TRANSFER)
            .unwrap_err();
        assert_eq!(err, IpcError::BadHandle);
        assert_eq!(table.live_count(), 2);
    }

    #[test]
    fn tables_are_independent() {
        let mut a = HandleTable::new();
        let mut b = HandleTable::new();

        let id_a = a.insert(port_handle(Rights::READ)).unwrap();
        let throwaway = b.insert(port_handle(Rights::WRITE)).unwrap();
        b.remove(throwaway).unwrap();
        let id_b = b.insert(port_handle(Rights::WRITE)).unwrap();

        assert!(a.get(id_a, Rights::READ).is_ok());
        assert!(b.get(id_b, Rights::WRITE).is_ok());
        assert_ne!(id_a.generation(), id_b.generation());
        assert_eq!(a.get(id_b, Rights::READ).unwrap_err(), IpcError::BadHandle);
    }

    #[test]
    fn register_cancel_then_remove_fires_cancel() {
        let mut table = HandleTable::new();
        let id = table.insert(signal_handle(Rights::READ)).unwrap();
        let target = CountingCancel::new();
        let dyn_target: Arc<dyn CancelTarget> = target.clone();

        table.register_cancel(id, dyn_target).unwrap();
        assert_eq!(target.count(), 0);

        let _h = table.remove(id).unwrap();
        assert_eq!(target.count(), 1);
    }

    #[test]
    fn register_cancel_supports_multiple_targets_per_slot() {
        let mut table = HandleTable::new();
        let id = table.insert(signal_handle(Rights::READ)).unwrap();
        let a = CountingCancel::new();
        let b = CountingCancel::new();
        table
            .register_cancel(id, a.clone() as Arc<dyn CancelTarget>)
            .unwrap();
        table
            .register_cancel(id, b.clone() as Arc<dyn CancelTarget>)
            .unwrap();

        let _h = table.remove(id).unwrap();
        assert_eq!(a.count(), 1);
        assert_eq!(b.count(), 1);
    }

    #[test]
    fn register_cancel_on_bad_handle_returns_err() {
        let mut table = HandleTable::new();
        let bogus = HandleId::pack(1, 999);
        let target: Arc<dyn CancelTarget> = CountingCancel::new();
        assert_eq!(
            table.register_cancel(bogus, target).unwrap_err(),
            IpcError::BadHandle
        );
    }

    #[test]
    fn register_cancel_after_close_returns_err() {
        let mut table = HandleTable::new();
        let id = table.insert(signal_handle(Rights::READ)).unwrap();
        table.remove(id).unwrap();
        let target: Arc<dyn CancelTarget> = CountingCancel::new();
        assert_eq!(
            table.register_cancel(id, target).unwrap_err(),
            IpcError::BadHandle
        );
    }

    #[test]
    fn unregister_cancel_after_normal_wakeup_drops_target() {
        let mut table = HandleTable::new();
        let id = table.insert(signal_handle(Rights::READ)).unwrap();
        let target = CountingCancel::new();
        let dyn_target: Arc<dyn CancelTarget> = target.clone();
        table.register_cancel(id, dyn_target.clone()).unwrap();
        table.unregister_cancel(id, &dyn_target);

        let _h = table.remove(id).unwrap();
        assert_eq!(target.count(), 0);
    }

    #[test]
    fn unregister_cancel_idempotent_and_tolerates_missing() {
        let mut table = HandleTable::new();
        let id = table.insert(signal_handle(Rights::READ)).unwrap();
        let target = CountingCancel::new();
        let dyn_target: Arc<dyn CancelTarget> = target.clone();
        table.register_cancel(id, dyn_target.clone()).unwrap();
        table.unregister_cancel(id, &dyn_target);
        table.unregister_cancel(id, &dyn_target);
        let bogus = HandleId::pack(99, 999);
        table.unregister_cancel(bogus, &dyn_target);
    }

    #[test]
    fn try_drain_for_transfer_fires_cancel_for_each_handle() {
        let mut table = HandleTable::new();
        let id1 = table
            .insert(signal_handle(Rights::READ | Rights::TRANSFER))
            .unwrap();
        let id2 = table
            .insert(signal_handle(Rights::READ | Rights::TRANSFER))
            .unwrap();
        let a = CountingCancel::new();
        let b = CountingCancel::new();
        table
            .register_cancel(id1, a.clone() as Arc<dyn CancelTarget>)
            .unwrap();
        table
            .register_cancel(id2, b.clone() as Arc<dyn CancelTarget>)
            .unwrap();

        let drained = table
            .try_drain_for_transfer(&[id1, id2], Rights::TRANSFER)
            .unwrap();
        assert_eq!(drained.len(), 2);
        assert_eq!(a.count(), 1);
        assert_eq!(b.count(), 1);
    }

    #[test]
    fn slot_reuse_does_not_carry_over_old_waiters() {
        let mut table = HandleTable::with_capacity(1);
        let id1 = table.insert(signal_handle(Rights::READ)).unwrap();
        let target = CountingCancel::new();
        table
            .register_cancel(id1, target.clone() as Arc<dyn CancelTarget>)
            .unwrap();
        table.remove(id1).unwrap();
        assert_eq!(target.count(), 1);

        let id2 = table.insert(signal_handle(Rights::READ)).unwrap();
        assert_eq!(id1.slot(), id2.slot());
        table.remove(id2).unwrap();
        assert_eq!(target.count(), 1);
    }

    #[test]
    fn reserve_slot_returns_out_of_handles_when_full() {
        let mut table = HandleTable::with_capacity(1);
        table.insert(signal_handle(Rights::READ)).unwrap();
        assert_eq!(table.reserve_slot().unwrap_err(), IpcError::OutOfHandles);
    }

    #[test]
    fn commit_reserved_inserts_handle_and_returns_predicted_id() {
        let mut table = HandleTable::with_capacity(1);
        let reservation = table.reserve_slot().expect("reserve must succeed");
        let predicted = reservation.handle_id();
        let id = table.commit_reserved(reservation, signal_handle(Rights::READ));
        assert_eq!(id, predicted);
        assert!(table.get(id, Rights::READ).is_ok());
        assert_eq!(table.live_count(), 1);
    }

    #[test]
    fn release_reservation_returns_slot_to_free_list() {
        let mut table = HandleTable::with_capacity(1);
        let reservation = table.reserve_slot().expect("reserve must succeed");
        table.release_reservation(reservation);

        let id = table
            .insert(signal_handle(Rights::READ))
            .expect("insert after release must succeed");
        assert!(table.get(id, Rights::READ).is_ok());
    }

    #[test]
    fn reserved_slot_is_not_visible_via_lookup() {
        let mut table = HandleTable::with_capacity(1);
        let reservation = table.reserve_slot().expect("reserve");
        let predicted = reservation.handle_id();
        assert_eq!(
            table.get(predicted, Rights::empty()).unwrap_err(),
            IpcError::BadHandle,
        );
        assert_eq!(table.live_count(), 0);
        table.release_reservation(reservation);
    }

    #[test]
    fn reserve_slot_succeeds_after_drain_on_full_table() {
        let mut table = HandleTable::with_capacity(2);
        let id1 = table
            .insert(signal_handle(Rights::READ | Rights::TRANSFER))
            .expect("insert 1");
        table
            .insert(signal_handle(Rights::READ | Rights::TRANSFER))
            .expect("insert 2");
        assert_eq!(table.reserve_slot().unwrap_err(), IpcError::OutOfHandles);

        let _drained = table
            .try_drain_for_transfer(&[id1], Rights::TRANSFER)
            .expect("drain");

        let reservation = table
            .reserve_slot()
            .expect("post-drain reserve_slot must succeed");
        table.commit_reserved(reservation, signal_handle(Rights::READ));
    }

    #[test]
    fn release_reservation_does_not_burn_generation() {
        let mut table = HandleTable::with_capacity(1);
        let seed_id = table.insert(signal_handle(Rights::READ)).expect("insert");
        let seed_gen = seed_id.generation();
        table.remove(seed_id).expect("remove");

        // Многократный неуспешный reserve+release не должен жечь generation:
        // ни один HandleId не публиковался, поэтому слот не нужно ретайрить.
        for _ in 0..32 {
            let r = table.reserve_slot().expect("reserve");
            table.release_reservation(r);
        }

        let final_id = table.insert(signal_handle(Rights::READ)).expect("insert");
        assert_eq!(
            final_id.generation(),
            seed_gen + 1,
            "лишь один реальный commit должен сдвинуть generation",
        );
    }

    fn transfer(src: &mut HandleTable, dst: &mut HandleTable, ids: &[HandleId]) -> Vec<HandleId> {
        let reservations: Vec<HandleReservation> = ids
            .iter()
            .map(|_| dst.reserve_slot().expect("reserve"))
            .collect();
        let drained = src
            .try_drain_for_transfer(ids, Rights::TRANSFER)
            .expect("drain");
        reservations
            .into_iter()
            .zip(drained)
            .map(|(res, h)| dst.commit_reserved(res, h))
            .collect()
    }

    fn dup_rights() -> Rights {
        Rights::DUPLICATE | Rights::READ | Rights::TRANSFER
    }

    #[test]
    fn lookup_revoked_after_grantor_closed_across_transfer() {
        let mut grantor = HandleTable::new();
        let mut recipient = HandleTable::new();

        let root = grantor.insert(signal_handle(dup_rights())).unwrap();
        let derived = grantor.duplicate(root, dup_rights(), 0).unwrap();
        let recv_id = transfer(&mut grantor, &mut recipient, &[derived])[0];

        assert!(recipient.get(recv_id, Rights::READ).is_ok());

        grantor.remove(root).unwrap();
        assert_eq!(
            recipient.get(recv_id, Rights::READ).unwrap_err(),
            IpcError::Revoked
        );
    }

    #[test]
    fn transfer_of_root_is_not_revocable() {
        let mut grantor = HandleTable::new();
        let mut recipient = HandleTable::new();

        let root = grantor.insert(signal_handle(dup_rights())).unwrap();
        let recv_id = transfer(&mut grantor, &mut recipient, &[root])[0];

        assert!(recipient.get(recv_id, Rights::READ).is_ok());
    }

    #[test]
    fn granularity_via_intermediate_nodes() {
        let mut grantor = HandleTable::new();
        let mut client_p = HandleTable::new();
        let mut client_q = HandleTable::new();

        let root = grantor.insert(signal_handle(dup_rights())).unwrap();
        let node_p = grantor.duplicate(root, dup_rights(), 0).unwrap();
        let node_q = grantor.duplicate(root, dup_rights(), 0).unwrap();

        let give_p = grantor.duplicate(node_p, dup_rights(), 0).unwrap();
        let give_q = grantor.duplicate(node_q, dup_rights(), 0).unwrap();
        let p_id = transfer(&mut grantor, &mut client_p, &[give_p])[0];
        let q_id = transfer(&mut grantor, &mut client_q, &[give_q])[0];

        grantor.remove(node_q).unwrap();

        assert_eq!(
            client_q.get(q_id, Rights::READ).unwrap_err(),
            IpcError::Revoked
        );
        assert!(client_p.get(p_id, Rights::READ).is_ok());
    }

    #[test]
    fn closing_root_revokes_whole_subtree() {
        let mut grantor = HandleTable::new();
        let mut client_p = HandleTable::new();
        let mut client_q = HandleTable::new();

        let root = grantor.insert(signal_handle(dup_rights())).unwrap();
        let give_p = grantor.duplicate(root, dup_rights(), 0).unwrap();
        let give_q = grantor.duplicate(root, dup_rights(), 0).unwrap();
        let p_id = transfer(&mut grantor, &mut client_p, &[give_p])[0];
        let q_id = transfer(&mut grantor, &mut client_q, &[give_q])[0];

        grantor.remove(root).unwrap();

        assert_eq!(
            client_p.get(p_id, Rights::READ).unwrap_err(),
            IpcError::Revoked
        );
        assert_eq!(
            client_q.get(q_id, Rights::READ).unwrap_err(),
            IpcError::Revoked
        );
    }

    #[test]
    fn duplicate_of_revoked_handle_fails() {
        let mut grantor = HandleTable::new();
        let mut recipient = HandleTable::new();

        let root = grantor.insert(signal_handle(dup_rights())).unwrap();
        let derived = grantor.duplicate(root, dup_rights(), 0).unwrap();
        let recv_id = transfer(&mut grantor, &mut recipient, &[derived])[0];
        grantor.remove(root).unwrap();

        assert_eq!(
            recipient.duplicate(recv_id, Rights::READ, 0).unwrap_err(),
            IpcError::Revoked
        );
    }

    #[test]
    fn revoked_slot_can_still_be_closed() {
        let mut grantor = HandleTable::new();
        let mut recipient = HandleTable::new();

        let root = grantor.insert(signal_handle(dup_rights())).unwrap();
        let derived = grantor.duplicate(root, dup_rights(), 0).unwrap();
        let recv_id = transfer(&mut grantor, &mut recipient, &[derived])[0];
        grantor.remove(root).unwrap();

        assert!(recipient.remove(recv_id).is_ok());
        assert_eq!(recipient.live_count(), 0);
    }

    #[test]
    fn ancestors_and_siblings_keep_access_after_descendant_close() {
        let mut table = HandleTable::new();
        let root = table.insert(signal_handle(dup_rights())).unwrap();
        let child_a = table.duplicate(root, dup_rights(), 0).unwrap();
        let child_b = table.duplicate(root, dup_rights(), 0).unwrap();

        table.remove(child_a).unwrap();

        assert!(table.get(root, Rights::READ).is_ok());
        assert!(table.get(child_b, Rights::READ).is_ok());
    }

    #[test]
    fn registered_hook_fires_on_close() {
        use alloc::sync::Weak;

        use super::super::rev_node::RevocationHook;

        struct CountingHook(AtomicUsize);
        impl RevocationHook for CountingHook {
            fn revoke(&self) {
                self.0.fetch_add(1, Ordering::AcqRel);
            }
        }

        let mut table = HandleTable::new();
        let id = table.insert(signal_handle(dup_rights())).unwrap();
        let hook = Arc::new(CountingHook(AtomicUsize::new(0)));
        table
            .register_revocation_hook(id, Arc::downgrade(&hook) as Weak<dyn RevocationHook>)
            .unwrap();
        assert_eq!(hook.0.load(Ordering::Acquire), 0);

        table.remove(id).unwrap();
        assert_eq!(hook.0.load(Ordering::Acquire), 1);
    }

    #[test]
    fn registered_hook_fires_for_descendant_on_ancestor_close() {
        use alloc::sync::Weak;

        use super::super::rev_node::RevocationHook;

        struct CountingHook(AtomicUsize);
        impl RevocationHook for CountingHook {
            fn revoke(&self) {
                self.0.fetch_add(1, Ordering::AcqRel);
            }
        }

        let mut grantor = HandleTable::new();
        let mut recipient = HandleTable::new();
        let root = grantor.insert(signal_handle(dup_rights())).unwrap();
        let derived = grantor.duplicate(root, dup_rights(), 0).unwrap();
        let recv_id = transfer(&mut grantor, &mut recipient, &[derived])[0];

        let hook = Arc::new(CountingHook(AtomicUsize::new(0)));
        recipient
            .register_revocation_hook(recv_id, Arc::downgrade(&hook) as Weak<dyn RevocationHook>)
            .unwrap();

        grantor.remove(root).unwrap();
        assert_eq!(hook.0.load(Ordering::Acquire), 1);
    }

    #[test]
    fn table_drop_revokes_derived_in_other_table() {
        let mut recipient = HandleTable::new();
        let recv_id;

        {
            let mut grantor = HandleTable::new();
            let root = grantor.insert(signal_handle(dup_rights())).unwrap();
            let derived = grantor.duplicate(root, dup_rights(), 0).unwrap();
            recv_id = transfer(&mut grantor, &mut recipient, &[derived])[0];
            assert!(recipient.get(recv_id, Rights::READ).is_ok());
        }

        assert_eq!(
            recipient.get(recv_id, Rights::READ).unwrap_err(),
            IpcError::Revoked
        );
    }
}
