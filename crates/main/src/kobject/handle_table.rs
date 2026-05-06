use alloc::{sync::Arc, vec::Vec};

use super::{
    channel::ChannelEndpoint,
    errors::IpcError,
    event::Event,
    handle::{Handle, HandleId},
    object::KObject,
    rights::Rights,
};

/// Стандартная ёмкость таблицы handle'ов одного процесса.
const DEFAULT_CAPACITY: u32 = 1024;

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
}

enum SlotState {
    /// Слот свободен, ссылка на следующий элемент free-list'а.
    Free { next_free: Option<u32> },
    /// Слот занят живым handle'ом.
    Occupied(Handle),
    /// Generation исчерпана; слот выведен из оборота навсегда.
    Retired,
}

impl HandleTable {
    pub fn new() -> Self {
        Self::with_capacity(DEFAULT_CAPACITY)
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
    pub fn insert(&mut self, handle: Handle) -> Result<HandleId, IpcError> {
        if let Some(idx) = self.pop_free_slot() {
            let slot = &mut self.slots[idx as usize];
            // pop_free_slot гарантирует, что generation ещё не исчерпана.
            slot.generation += 1;
            slot.state = SlotState::Occupied(handle);
            return Ok(HandleId::pack(slot.generation, idx));
        }

        if (self.slots.len() as u32) >= self.capacity {
            return Err(IpcError::OutOfHandles);
        }

        let idx = self.slots.len() as u32;
        self.slots.push(Slot {
            generation: 1,
            state: SlotState::Occupied(handle),
        });
        Ok(HandleId::pack(1, idx))
    }

    /// Удаляет handle из таблицы, возвращая его (вызывающий решает, что
    /// делать с `Arc` - обычно дроп закрывает KO).
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
        self.free_head = Some(idx);

        match taken {
            SlotState::Occupied(handle) => Ok(handle),
            // Невозможно по проверке выше, но обходимся без unreachable!.
            _ => Err(IpcError::BadHandle),
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

    /// Извлекает `Arc<ChannelEndpoint>` с проверкой прав и типа.
    pub fn get_channel(
        &self,
        id: HandleId,
        need: Rights,
    ) -> Result<Arc<ChannelEndpoint>, IpcError> {
        let h = self.lookup(id)?;
        if !h.rights().contains(need) {
            return Err(IpcError::AccessDenied);
        }
        match &h.object {
            KObject::Channel(c) => Ok(c.clone()),
            KObject::Event(_) => Err(IpcError::WrongType),
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
            KObject::Channel(_) => Err(IpcError::WrongType),
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
            SlotState::Free { .. } | SlotState::Retired => Err(IpcError::BadHandle),
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
    use super::{
        super::{event::Event, object::KObject, rights::Rights},
        *,
    };

    fn make_handle(obj: KObject, rights: Rights) -> Handle {
        Handle::new(obj, rights)
    }

    fn event_handle(rights: Rights) -> Handle {
        make_handle(KObject::Event(Event::new()), rights)
    }

    fn channel_handle(rights: Rights) -> Handle {
        let (ep, _) = ChannelEndpoint::create_pair(4);
        make_handle(KObject::Channel(ep), rights)
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

        // Верный тип проходит.
        assert!(table.get_event(event_id, Rights::WAIT).is_ok());
        assert!(table.get_channel(chan_id, Rights::READ).is_ok());

        // Неверный тип возвращает WrongType.
        assert!(matches!(
            table.get_channel(event_id, Rights::WAIT),
            Err(IpcError::WrongType)
        ));
        assert!(matches!(
            table.get_event(chan_id, Rights::READ),
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
}
