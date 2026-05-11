use alloc::{sync::Arc, vec::Vec};

use memory::MemoryRegion;

use super::{
    channel::Channel,
    errors::IpcError,
    event::Event,
    handle::{Handle, HandleId},
    mailbox::Mailbox,
    object::KObject,
    physical_resource::PhysicalResource,
    process::ProcessObject,
    rights::Rights,
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
    /// Текущая generation слота, 1..=`MAX_GENERATION`.
    generation: u16,
    state: SlotState,
    /// Cancel-target'ы активных wait'ов; дренируются при `Occupied -> Free`.
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

    /// Текущее число занятых слотов. O(n) - используется только в тестах
    /// и diagnostics-путях.
    pub fn live_count(&self) -> usize {
        self.slots
            .iter()
            .filter(|s| matches!(s.state, SlotState::Occupied(_)))
            .count()
    }

    /// Регистрирует handle, возвращая стабильный идентификатор.
    /// На `OutOfHandles` объект **закрывается** (Handle уходит в drop).
    /// Если caller'у важно сохранить объект на ошибке, используется
    /// [`Self::try_insert`].
    pub fn insert(&mut self, handle: Handle) -> Result<HandleId, IpcError> {
        self.try_insert(handle).map_err(|(e, _)| e)
    }

    /// То же, что [`Self::insert`], но при `OutOfHandles` возвращает
    /// `Handle` обратно вместо его закрытия. Нужен для путей с
    /// rollback'ом - например, atomic-`ChannelRead`.
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
    /// Generation, использованная резервацией, не жжётся: слот не публиковал
    /// `HandleId`, поэтому пред-резервационное значение можно переиспользовать.
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

    /// Удаляет handle из таблицы. Дренирует per-slot cancel-target'ы
    /// и будит каждого с исходом
    /// [`IpcError::Canceled`](super::IpcError::Canceled).
    pub fn remove(&mut self, id: HandleId) -> Result<Handle, IpcError> {
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

        // cancel() -> runtime.unblock(); runtime не входит обратно в
        // HandleTable, лок безопасно держать.
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

    /// Снимает cancel-target по identity. Идемпотентен; generation
    /// не проверяется - переиспользованный слот заведомо не содержит
    /// чужой Arc.
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

    /// Проверка прав без проверки типа. Type-check делается callers
    /// через match на `handle.object()` или через type-specific accessor.
    pub fn get(&self, id: HandleId, need: Rights) -> Result<&Handle, IpcError> {
        let handle = self.lookup(id)?;
        if !handle.rights().contains(need) {
            return Err(IpcError::AccessDenied);
        }
        Ok(handle)
    }

    /// Извлекает `Arc<Channel>` с проверкой прав и типа.
    pub fn get_channel(&self, id: HandleId, need: Rights) -> Result<Arc<Channel>, IpcError> {
        let h = self.lookup(id)?;
        if !h.rights().contains(need) {
            return Err(IpcError::AccessDenied);
        }
        match &h.object {
            KObject::Channel(c) => Ok(c.clone()),
            KObject::Event(_)
            | KObject::Process(_)
            | KObject::Thread(_)
            | KObject::Memory(_)
            | KObject::PhysicalResource(_)
            | KObject::Mailbox(_) => Err(IpcError::WrongType),
        }
    }

    /// Извлекает `Arc<Event>` с проверкой прав и типа.
    pub fn get_event(&self, id: HandleId, need: Rights) -> Result<Arc<Event>, IpcError> {
        let h = self.lookup(id)?;
        if !h.rights().contains(need) {
            return Err(IpcError::AccessDenied);
        }
        match &h.object {
            KObject::Event(e) => Ok(e.clone()),
            KObject::Channel(_)
            | KObject::Process(_)
            | KObject::Thread(_)
            | KObject::Memory(_)
            | KObject::PhysicalResource(_)
            | KObject::Mailbox(_) => Err(IpcError::WrongType),
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
            KObject::Channel(_)
            | KObject::Event(_)
            | KObject::Thread(_)
            | KObject::Memory(_)
            | KObject::PhysicalResource(_)
            | KObject::Mailbox(_) => Err(IpcError::WrongType),
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
            KObject::Channel(_)
            | KObject::Event(_)
            | KObject::Process(_)
            | KObject::Memory(_)
            | KObject::PhysicalResource(_)
            | KObject::Mailbox(_) => Err(IpcError::WrongType),
        }
    }

    /// Извлекает `Arc<MemoryRegion>` с проверкой прав и типа.
    pub fn get_memory(&self, id: HandleId, need: Rights) -> Result<Arc<MemoryRegion>, IpcError> {
        self.get_memory_with_rights(id, need).map(|(m, _)| m)
    }

    /// Извлекает `Arc<MemoryRegion>` вместе с полным набором `Rights` handle'а.
    /// Полный набор нужен `MemoryMap` для вычисления `grant` - потолка
    /// access-битов, разрешённых caller'у на этом mapping'е.
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
            KObject::Channel(_)
            | KObject::Event(_)
            | KObject::Process(_)
            | KObject::Thread(_)
            | KObject::PhysicalResource(_)
            | KObject::Mailbox(_) => Err(IpcError::WrongType),
        }
    }

    /// Извлекает `Arc<PhysicalResource>` с проверкой прав и типа.
    pub fn get_physical_resource(
        &self,
        id: HandleId,
        need: Rights,
    ) -> Result<Arc<PhysicalResource>, IpcError> {
        let h = self.lookup(id)?;
        if !h.rights().contains(need) {
            return Err(IpcError::AccessDenied);
        }
        match &h.object {
            KObject::PhysicalResource(r) => Ok(r.clone()),
            KObject::Channel(_)
            | KObject::Event(_)
            | KObject::Process(_)
            | KObject::Thread(_)
            | KObject::Memory(_)
            | KObject::Mailbox(_) => Err(IpcError::WrongType),
        }
    }

    /// Извлекает `Arc<Mailbox>` с проверкой прав и типа.
    pub fn get_mailbox(&self, id: HandleId, need: Rights) -> Result<Arc<Mailbox>, IpcError> {
        let h = self.lookup(id)?;
        if !h.rights().contains(need) {
            return Err(IpcError::AccessDenied);
        }
        match &h.object {
            KObject::Mailbox(m) => Ok(m.clone()),
            KObject::Channel(_)
            | KObject::Event(_)
            | KObject::Process(_)
            | KObject::Thread(_)
            | KObject::Memory(_)
            | KObject::PhysicalResource(_) => Err(IpcError::WrongType),
        }
    }

    /// Доступ к KO без проверки конкретного типа - для wait-пути,
    /// который применим к любому signalable (Channel/Event/Timer/...).
    /// Клонирует `KObject`, чтобы caller мог работать с объектом вне
    /// HandleTable-lock'а.
    pub fn clone_object(&self, id: HandleId, need: Rights) -> Result<KObject, IpcError> {
        let handle = self.lookup(id)?;
        if !handle.rights().contains(need) {
            return Err(IpcError::AccessDenied);
        }
        Ok(handle.object().clone())
    }

    /// Атомарно проверяет, что все `ids` существуют, имеют `min_rights`
    /// и нет дубликатов; затем удаляет их и возвращает `Vec<Handle>`.
    ///
    /// Атомарность важна для syscall handle-transfer: пользователь
    /// должен либо потерять все handle'ы (msg уехал), либо ни одного
    /// (build упал). Любая ошибка валидации - таблица не модифицируется.
    pub fn try_drain_for_transfer(
        &mut self,
        ids: &[HandleId],
        min_rights: Rights,
    ) -> Result<Vec<Handle>, IpcError> {
        // Дубликаты в `ids` отвергаем явно: после `remove(first)`
        // повторный поиск второго вернул бы BadHandle "по факту", но
        // ошибка относится не к закрытому handle'у, а к малформированному
        // запросу - проверяем заранее, чтобы корректно атомарно отказать.
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
            // Валидация выше гарантирует, что remove не упадёт: id найден,
            // generation совпадает, rights включают min_rights.
            let h = self.remove(*id).expect("validated above");
            drained.push(h);
        }
        Ok(drained)
    }

    /// Создаёт новый handle на тот же KO с подмножеством прав.
    pub fn duplicate(&mut self, id: HandleId, new_rights: Rights) -> Result<HandleId, IpcError> {
        let dup = {
            let handle = self.lookup(id)?;
            handle.duplicate(new_rights)?
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
            SlotState::Occupied(handle) => Ok(handle),
            SlotState::Free { .. } | SlotState::Reserved | SlotState::Retired => {
                Err(IpcError::BadHandle)
            }
        }
    }

    /// Снимает с free-list'а первый слот, у которого ещё не исчерпана
    /// generation. Слоты с исчерпанной generation помечаются `Retired`
    /// и выводятся из оборота.
    fn pop_free_slot(&mut self) -> Option<u32> {
        while let Some(idx) = self.free_head {
            let slot = &mut self.slots[idx as usize];
            // free_head обязан указывать на Free-слот. Если это не так
            // (корруптный free-list) - обрываем обход, последующие
            // insert'ы пойдут по пути "новый слот".
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

#[cfg(test)]
mod tests {
    use core::sync::atomic::{AtomicUsize, Ordering};

    use super::{
        super::{
            event::Event, mailbox::Mailbox, object::KObject, process::ProcessObject,
            rights::Rights, thread::ThreadObject, wait::CancelTarget,
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

    fn event_handle(rights: Rights) -> Handle {
        make_handle(KObject::Event(Event::new()), rights)
    }

    fn channel_handle(rights: Rights) -> Handle {
        let (ep, _) = Channel::create_pair(4);
        make_handle(KObject::Channel(ep), rights)
    }

    fn process_handle(rights: Rights) -> Handle {
        make_handle(KObject::Process(ProcessObject::new()), rights)
    }

    fn thread_handle(rights: Rights) -> Handle {
        make_handle(KObject::Thread(ThreadObject::new()), rights)
    }

    fn mailbox_handle(rights: Rights) -> Handle {
        make_handle(KObject::Mailbox(Mailbox::new()), rights)
    }

    #[test]
    fn insert_and_get_round_trip() {
        let mut table = HandleTable::new();
        let h = channel_handle(Rights::READ | Rights::WRITE);
        let koid = h.koid();

        let id = table.insert(h).unwrap();
        let got = table.get(id, Rights::READ).expect("get must succeed");
        assert_eq!(got.koid(), koid);
        assert_eq!(table.live_count(), 1);
    }

    #[test]
    fn get_with_missing_right_returns_access_denied() {
        let mut table = HandleTable::new();
        let id = table.insert(event_handle(Rights::WAIT)).unwrap();

        assert_eq!(
            table.get(id, Rights::SIGNAL).unwrap_err(),
            IpcError::AccessDenied
        );
        // Право, которое реально есть, проходит.
        assert!(table.get(id, Rights::WAIT).is_ok());
    }

    #[test]
    fn get_channel_type_checks() {
        let mut table = HandleTable::new();
        let event_id = table.insert(event_handle(Rights::WAIT)).unwrap();
        let chan_id = table.insert(channel_handle(Rights::READ)).unwrap();
        let proc_id = table.insert(process_handle(Rights::WAIT)).unwrap();
        let thread_id = table.insert(thread_handle(Rights::WAIT)).unwrap();
        let mbox_id = table.insert(mailbox_handle(Rights::READ)).unwrap();

        // Верный тип проходит.
        assert!(table.get_event(event_id, Rights::WAIT).is_ok());
        assert!(table.get_channel(chan_id, Rights::READ).is_ok());
        assert!(table.get_mailbox(mbox_id, Rights::READ).is_ok());

        // Неверный тип возвращает WrongType.
        assert!(matches!(
            table.get_channel(event_id, Rights::WAIT),
            Err(IpcError::WrongType)
        ));
        assert!(matches!(
            table.get_event(chan_id, Rights::READ),
            Err(IpcError::WrongType)
        ));
        assert!(matches!(
            table.get_channel(proc_id, Rights::WAIT),
            Err(IpcError::WrongType)
        ));
        assert!(matches!(
            table.get_event(thread_id, Rights::WAIT),
            Err(IpcError::WrongType)
        ));
        assert!(matches!(
            table.get_mailbox(chan_id, Rights::READ),
            Err(IpcError::WrongType)
        ));
        assert!(matches!(
            table.get_channel(mbox_id, Rights::READ),
            Err(IpcError::WrongType)
        ));
    }

    #[test]
    fn get_process_type_checks() {
        let mut table = HandleTable::new();
        let proc_id = table.insert(process_handle(Rights::WAIT)).unwrap();
        let event_id = table.insert(event_handle(Rights::WAIT)).unwrap();
        let chan_id = table.insert(channel_handle(Rights::READ)).unwrap();
        let thread_id = table.insert(thread_handle(Rights::WAIT)).unwrap();

        assert!(table.get_process(proc_id, Rights::WAIT).is_ok());
        assert!(matches!(
            table.get_process(event_id, Rights::WAIT),
            Err(IpcError::WrongType)
        ));
        assert!(matches!(
            table.get_process(chan_id, Rights::READ),
            Err(IpcError::WrongType)
        ));
        assert!(matches!(
            table.get_process(thread_id, Rights::WAIT),
            Err(IpcError::WrongType)
        ));
    }

    #[test]
    fn get_thread_type_checks() {
        let mut table = HandleTable::new();
        let thread_id = table.insert(thread_handle(Rights::WAIT)).unwrap();
        let event_id = table.insert(event_handle(Rights::WAIT)).unwrap();
        let chan_id = table.insert(channel_handle(Rights::READ)).unwrap();
        let proc_id = table.insert(process_handle(Rights::WAIT)).unwrap();

        assert!(table.get_thread(thread_id, Rights::WAIT).is_ok());
        assert!(matches!(
            table.get_thread(event_id, Rights::WAIT),
            Err(IpcError::WrongType)
        ));
        assert!(matches!(
            table.get_thread(chan_id, Rights::READ),
            Err(IpcError::WrongType)
        ));
        assert!(matches!(
            table.get_thread(proc_id, Rights::WAIT),
            Err(IpcError::WrongType)
        ));
    }

    #[test]
    fn get_process_checks_rights() {
        let mut table = HandleTable::new();
        let id = table.insert(process_handle(Rights::WAIT)).unwrap();
        assert!(matches!(
            table.get_process(id, Rights::MANAGE_PROCESS),
            Err(IpcError::AccessDenied)
        ));
        assert!(table.get_process(id, Rights::WAIT).is_ok());
    }

    #[test]
    fn get_thread_checks_rights() {
        let mut table = HandleTable::new();
        let id = table.insert(thread_handle(Rights::WAIT)).unwrap();
        assert!(matches!(
            table.get_thread(id, Rights::MANAGE_THREAD),
            Err(IpcError::AccessDenied)
        ));
        assert!(table.get_thread(id, Rights::WAIT).is_ok());
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
        let mem_rights = Rights::MAP | Rights::READ | Rights::WAIT;
        let mem_id = table
            .insert(make_handle(KObject::Memory(region), mem_rights))
            .unwrap();
        let event_id = table.insert(event_handle(Rights::WAIT)).unwrap();
        let chan_id = table.insert(channel_handle(Rights::READ)).unwrap();

        assert!(table.get_memory(mem_id, Rights::MAP).is_ok());
        assert!(matches!(
            table.get_memory(event_id, Rights::WAIT),
            Err(IpcError::WrongType)
        ));
        assert!(matches!(
            table.get_memory(chan_id, Rights::READ),
            Err(IpcError::WrongType)
        ));
        assert!(matches!(
            table.get_channel(mem_id, Rights::READ),
            Err(IpcError::WrongType)
        ));
        assert!(matches!(
            table.get_event(mem_id, Rights::WAIT),
            Err(IpcError::WrongType)
        ));
    }

    #[test]
    fn get_physical_resource_type_checks() {
        use core::num::NonZeroUsize;

        use memory::{AccessMask, physical_address::PageAlignedAddress};

        use super::super::physical_resource::PhysicalResource;

        let resource = PhysicalResource::new(
            PageAlignedAddress::from_usize(0x4000_0000).unwrap(),
            NonZeroUsize::new(4096).unwrap(),
            AccessMask::RW,
        );
        let mut table = HandleTable::new();
        let res_rights = Rights::MINT | Rights::WAIT;
        let res_id = table
            .insert(make_handle(KObject::PhysicalResource(resource), res_rights))
            .unwrap();
        let event_id = table.insert(event_handle(Rights::WAIT)).unwrap();
        let chan_id = table.insert(channel_handle(Rights::READ)).unwrap();

        assert!(table.get_physical_resource(res_id, Rights::MINT).is_ok());
        assert_eq!(
            table
                .get_physical_resource(res_id, Rights::MINT | Rights::READ)
                .unwrap_err(),
            IpcError::AccessDenied
        );
        assert!(matches!(
            table.get_physical_resource(event_id, Rights::WAIT),
            Err(IpcError::WrongType)
        ));
        assert!(matches!(
            table.get_physical_resource(chan_id, Rights::READ),
            Err(IpcError::WrongType)
        ));
        assert!(matches!(
            table.get_channel(res_id, Rights::WAIT),
            Err(IpcError::WrongType)
        ));
        assert!(matches!(
            table.get_event(res_id, Rights::WAIT),
            Err(IpcError::WrongType)
        ));
    }

    #[test]
    fn remove_then_get_returns_bad_handle() {
        let mut table = HandleTable::new();
        let id = table.insert(channel_handle(Rights::READ)).unwrap();

        let _ = table.remove(id).unwrap();
        assert_eq!(
            table.get(id, Rights::READ).unwrap_err(),
            IpcError::BadHandle
        );
    }

    #[test]
    fn double_close_returns_bad_handle() {
        let mut table = HandleTable::new();
        let id = table.insert(event_handle(Rights::WAIT)).unwrap();

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
        let id = table.insert(channel_handle(rights)).unwrap();

        let dup = table.duplicate(id, Rights::READ).unwrap();
        let got = table.get(dup, Rights::READ).unwrap();
        assert_eq!(got.rights(), Rights::READ);
        // У копии нет WRITE, хотя у оригинала был.
        assert_eq!(
            table.get(dup, Rights::WRITE).unwrap_err(),
            IpcError::AccessDenied
        );
        // Оригинал нетронут.
        assert!(table.get(id, Rights::WRITE).is_ok());
    }

    #[test]
    fn duplicate_rejects_extra_rights() {
        let mut table = HandleTable::new();
        let id = table
            .insert(channel_handle(Rights::DUPLICATE | Rights::READ))
            .unwrap();

        assert_eq!(
            table
                .duplicate(id, Rights::READ | Rights::WRITE)
                .unwrap_err(),
            IpcError::AccessDenied
        );
    }

    #[test]
    fn duplicate_requires_duplicate_right() {
        let mut table = HandleTable::new();
        let id = table.insert(channel_handle(Rights::READ)).unwrap();

        assert_eq!(
            table.duplicate(id, Rights::READ).unwrap_err(),
            IpcError::AccessDenied
        );
    }

    #[test]
    fn slot_reuse_invalidates_old_id() {
        let mut table = HandleTable::with_capacity(2);
        let id1 = table.insert(event_handle(Rights::WAIT)).unwrap();
        table.remove(id1).unwrap();

        // Тот же слот переиспользуется, но с новой generation.
        let id2 = table.insert(event_handle(Rights::WAIT)).unwrap();
        assert_eq!(id1.slot(), id2.slot());
        assert_ne!(id1.generation(), id2.generation());
        assert_eq!(
            table.get(id1, Rights::WAIT).unwrap_err(),
            IpcError::BadHandle
        );
        assert!(table.get(id2, Rights::WAIT).is_ok());
    }

    #[test]
    fn out_of_handles_when_capacity_reached() {
        let mut table = HandleTable::with_capacity(2);
        let _ = table.insert(event_handle(Rights::WAIT)).unwrap();
        let _ = table.insert(event_handle(Rights::WAIT)).unwrap();

        let err = table.insert(event_handle(Rights::WAIT)).unwrap_err();
        assert_eq!(err, IpcError::OutOfHandles);
    }

    /// `try_insert` возвращает Handle на ошибке (а не закрывает его).
    /// Это нужно atomic-rollback'у в `sys_channel_read`.
    #[test]
    fn try_insert_returns_handle_on_out_of_handles() {
        use alloc::sync::Arc;

        let mut table = HandleTable::with_capacity(1);
        table.insert(event_handle(Rights::WAIT)).unwrap();

        let event = Event::new();
        let weak = Arc::downgrade(&event);
        let handle = Handle::new(KObject::Event(event), Rights::WAIT);
        let (err, returned) = table.try_insert(handle).unwrap_err();
        assert_eq!(err, IpcError::OutOfHandles);
        // KO жив - handle вернулся caller'у, а не дропнут таблицей.
        assert!(weak.upgrade().is_some());
        // Можно положить его обратно в новую таблицу.
        let mut other = HandleTable::with_capacity(1);
        other.insert(returned).unwrap();
        assert_eq!(other.live_count(), 1);
    }

    #[test]
    fn try_insert_success_matches_insert() {
        let mut table = HandleTable::with_capacity(2);
        let id = table.try_insert(event_handle(Rights::WAIT)).unwrap();
        assert!(table.get(id, Rights::WAIT).is_ok());
    }

    #[test]
    fn generation_rollover_retires_slot() {
        // capacity=1: один слот, который мы будем переиспользовать
        // до исчерпания generation. После исчерпания insert обязан
        // вернуть OutOfHandles, потому что новый слот выделить негде,
        // а старый ушёл в Retired.
        let mut table = HandleTable::with_capacity(1);
        let max_gen = u32::from(HandleId::MAX_GENERATION);

        // первый insert - generation=1, остальные циклы - increment'ы
        let mut id = table.insert(event_handle(Rights::WAIT)).unwrap();
        assert_eq!(u32::from(id.generation()), 1);

        for expected_gen in 2..=max_gen {
            table.remove(id).unwrap();
            id = table.insert(event_handle(Rights::WAIT)).unwrap();
            assert_eq!(u32::from(id.generation()), expected_gen);
        }

        // generation = MAX, освобождаем - слот должен уйти в Retired,
        // следующий insert упирается в потолок ёмкости.
        table.remove(id).unwrap();
        assert_eq!(
            table.insert(event_handle(Rights::WAIT)).unwrap_err(),
            IpcError::OutOfHandles
        );
    }

    #[test]
    fn try_drain_for_transfer_success() {
        let mut table = HandleTable::new();
        let id1 = table
            .insert(event_handle(Rights::WAIT | Rights::TRANSFER))
            .unwrap();
        let id2 = table
            .insert(event_handle(Rights::WAIT | Rights::TRANSFER))
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
            .insert(event_handle(Rights::WAIT | Rights::TRANSFER))
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
        let id = table.insert(event_handle(Rights::WAIT)).unwrap();
        let err = table
            .try_drain_for_transfer(&[id], Rights::TRANSFER)
            .unwrap_err();
        assert_eq!(err, IpcError::AccessDenied);
        assert_eq!(table.live_count(), 1);
        assert!(table.get(id, Rights::WAIT).is_ok());
    }

    #[test]
    fn try_drain_for_transfer_duplicate_no_removal() {
        let mut table = HandleTable::new();
        let id = table
            .insert(event_handle(Rights::WAIT | Rights::TRANSFER))
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
        // Если третий id невалиден, первые два не должны быть изъяты.
        let mut table = HandleTable::new();
        let id1 = table
            .insert(event_handle(Rights::WAIT | Rights::TRANSFER))
            .unwrap();
        let id2 = table
            .insert(event_handle(Rights::WAIT | Rights::TRANSFER))
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
        // Две независимые таблицы изолируют свои слоты: id из B,
        // указывающий на ещё не существующий в A слот, должен дать
        // BadHandle, а не случайно попадать на чужой объект.
        let mut a = HandleTable::new();
        let mut b = HandleTable::new();

        let id_a = a.insert(channel_handle(Rights::READ)).unwrap();
        // Расходим слот в B так, чтобы его generation отличалась от A.
        let throwaway = b.insert(channel_handle(Rights::WRITE)).unwrap();
        b.remove(throwaway).unwrap();
        let id_b = b.insert(channel_handle(Rights::WRITE)).unwrap();

        assert!(a.get(id_a, Rights::READ).is_ok());
        assert!(b.get(id_b, Rights::WRITE).is_ok());
        assert_ne!(id_a.generation(), id_b.generation());
        assert_eq!(a.get(id_b, Rights::READ).unwrap_err(), IpcError::BadHandle);
    }

    #[test]
    fn register_cancel_then_remove_fires_cancel() {
        let mut table = HandleTable::new();
        let id = table.insert(event_handle(Rights::WAIT)).unwrap();
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
        let id = table.insert(event_handle(Rights::WAIT)).unwrap();
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
        let id = table.insert(event_handle(Rights::WAIT)).unwrap();
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
        let id = table.insert(event_handle(Rights::WAIT)).unwrap();
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
        let id = table.insert(event_handle(Rights::WAIT)).unwrap();
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
            .insert(event_handle(Rights::WAIT | Rights::TRANSFER))
            .unwrap();
        let id2 = table
            .insert(event_handle(Rights::WAIT | Rights::TRANSFER))
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
        let id1 = table.insert(event_handle(Rights::WAIT)).unwrap();
        let target = CountingCancel::new();
        table
            .register_cancel(id1, target.clone() as Arc<dyn CancelTarget>)
            .unwrap();
        table.remove(id1).unwrap();
        assert_eq!(target.count(), 1);

        let id2 = table.insert(event_handle(Rights::WAIT)).unwrap();
        assert_eq!(id1.slot(), id2.slot());
        table.remove(id2).unwrap();
        assert_eq!(target.count(), 1);
    }

    #[test]
    fn reserve_slot_returns_out_of_handles_when_full() {
        let mut table = HandleTable::with_capacity(1);
        table.insert(event_handle(Rights::WAIT)).unwrap();
        assert_eq!(table.reserve_slot().unwrap_err(), IpcError::OutOfHandles);
    }

    #[test]
    fn commit_reserved_inserts_handle_and_returns_predicted_id() {
        let mut table = HandleTable::with_capacity(1);
        let reservation = table.reserve_slot().expect("reserve must succeed");
        let predicted = reservation.handle_id();
        let id = table.commit_reserved(reservation, event_handle(Rights::WAIT));
        assert_eq!(id, predicted);
        assert!(table.get(id, Rights::WAIT).is_ok());
        assert_eq!(table.live_count(), 1);
    }

    #[test]
    fn release_reservation_returns_slot_to_free_list() {
        let mut table = HandleTable::with_capacity(1);
        let reservation = table.reserve_slot().expect("reserve must succeed");
        table.release_reservation(reservation);
        // После release слот доступен снова.
        let id = table
            .insert(event_handle(Rights::WAIT))
            .expect("insert after release must succeed");
        assert!(table.get(id, Rights::WAIT).is_ok());
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
        // Инвариант, на котором держится отложенный пост-drain reserve_slot
        // в `sys_process_start`: drain хоть одного handle'а гарантированно
        // освобождает слот, и последующий reserve_slot не отвергает запрос.
        let mut table = HandleTable::with_capacity(2);
        let id1 = table
            .insert(event_handle(Rights::WAIT | Rights::TRANSFER))
            .expect("insert 1");
        table
            .insert(event_handle(Rights::WAIT | Rights::TRANSFER))
            .expect("insert 2");
        assert_eq!(table.reserve_slot().unwrap_err(), IpcError::OutOfHandles);

        let _drained = table
            .try_drain_for_transfer(&[id1], Rights::TRANSFER)
            .expect("drain");

        let reservation = table
            .reserve_slot()
            .expect("post-drain reserve_slot must succeed");
        table.commit_reserved(reservation, event_handle(Rights::WAIT));
    }

    #[test]
    fn release_reservation_does_not_burn_generation() {
        let mut table = HandleTable::with_capacity(1);
        // Засеваем слот вставкой+remove, чтобы он лежал в free-list с
        // конкретной generation.
        let seed_id = table.insert(event_handle(Rights::WAIT)).expect("insert");
        let seed_gen = seed_id.generation();
        table.remove(seed_id).expect("remove");

        // Многократный неуспешный reserve+release не должен жечь generation:
        // ни один HandleId не публиковался, поэтому слот не нужно ретайрить.
        for _ in 0..32 {
            let r = table.reserve_slot().expect("reserve");
            table.release_reservation(r);
        }

        let final_id = table.insert(event_handle(Rights::WAIT)).expect("insert");
        assert_eq!(
            final_id.generation(),
            seed_gen + 1,
            "лишь один реальный commit должен сдвинуть generation",
        );
    }
}
