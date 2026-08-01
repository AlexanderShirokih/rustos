use alloc::{sync::Arc, vec::Vec};
use core::num::NonZeroU16;

use syscall::Rights;

use super::{
    errors::IpcError,
    handle::{Capability, HandleId},
    port::{Port, PortAccess},
    rev_node::{RevocationHook, revoke_subtree},
    target::{CapabilityTarget, TargetObject},
    wait::CancelTarget,
};

/// Per-process слот-таблица handle'ов.
///
/// Порядок блокировок: table-лок берётся под scheduler-локом, обратное ребро запрещено. Отзыв,
/// пробуждение cancel-целей, разрушение capability и снятие учёта берут scheduler-лок и потому под
/// table-локом не выполняются - их выносят [`DrainedHandles`], [`RejectedInsert`] и
/// [`PortAccess`](super::PortAccess).
pub struct HandleTable {
    slots: Vec<Slot>,
    free_head: Option<u32>,
    capacity: u32,
}

struct Slot {
    // 0 - поколение ещё не выдавалось: свежий слот либо откаченная резервация.
    generation: u16,
    state: SlotState,
    waiters: Vec<Arc<dyn CancelTarget>>,
}

impl Slot {
    fn fresh(state: SlotState) -> Self {
        Self {
            generation: 0,
            state,
            waiters: Vec::new(),
        }
    }

    // Насыщение недостижимо: pop_free_slot выводит слот из оборота на MAX_GENERATION.
    fn issue_generation(&mut self) -> NonZeroU16 {
        let issued = NonZeroU16::MIN.saturating_add(self.generation);
        self.generation = issued.get();
        issued
    }
}

enum SlotState {
    /// Слот свободен, ссылка на следующий элемент free-list'а.
    Free { next_free: Option<u32> },
    /// Слот удерживается под `commit_reserved`/`release_reservation`.
    Reserved,
    /// Слот занят живым handle'ом.
    Occupied(Capability),
    /// Generation исчерпана; слот выведен из оборота навсегда.
    Retired,
}

/// Изъятое из [`HandleTable`] содержимое: capability и cancel-цели их слотов. Освобождение через
/// [`DrainedHandles::release`] под table-локом не выполняется.
#[must_use = "DrainedHandles держит изъятые capability и cancel-цели"]
pub struct DrainedHandles {
    handles: Vec<Capability>,
    waiters: Vec<Arc<dyn CancelTarget>>,
}

impl DrainedHandles {
    pub fn empty() -> Self {
        Self {
            handles: Vec::new(),
            waiters: Vec::new(),
        }
    }

    /// Освобождает изъятое: ожидания слотов отменяются с
    /// [`IpcError::Canceled`](IpcError::Canceled), оставшиеся capability разрушаются вместе с
    /// поддеревьями.
    pub fn release(self) {
        for waiter in self.waiters {
            waiter.cancel();
        }
        for handle in &self.handles {
            revoke_subtree(handle.node());
        }
        drop(self.handles);
    }

    /// Отдаёт capability новому владельцу: поддеревья не отзываются. Cancel-цели остаются за
    /// [`Self::release`].
    pub fn take_handles(&mut self) -> Vec<Capability> {
        core::mem::take(&mut self.handles)
    }

    pub fn is_empty(&self) -> bool {
        self.handles.is_empty() && self.waiters.is_empty()
    }

    fn push_handle(&mut self, handle: Capability) {
        self.handles.push(handle);
    }

    fn push_waiters(&mut self, waiters: Vec<Arc<dyn CancelTarget>>) {
        self.waiters.extend(waiters);
    }
}

/// Отвергнутая вставка: причина отказа и невставленный объект. Разрушение через
/// [`RejectedInsert::release`] под table-локом не выполняется.
#[must_use = "RejectedInsert держит невставленный объект"]
pub struct RejectedInsert {
    error: IpcError,
    handle: Option<Capability>,
}

impl RejectedInsert {
    fn without_handle(error: IpcError) -> Self {
        Self {
            error,
            handle: None,
        }
    }

    /// Причина отказа; объект остаётся в токене.
    pub fn error(&self) -> IpcError {
        self.error
    }

    /// Разрушает невставленный объект и отдаёт причину отказа.
    pub fn release(self) -> IpcError {
        self.error
    }

    /// Забирает невставленный объект. `None`, если отказ наступил до его создания.
    pub fn into_handle(self) -> Option<Capability> {
        self.handle
    }
}

impl core::fmt::Debug for RejectedInsert {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        f.debug_struct("RejectedInsert")
            .field("error", &self.error)
            .finish_non_exhaustive()
    }
}

/// Объект типа `T` вместе с полным набором прав его handle'а
/// (см. [`HandleTable::get_object_with_rights`]).
pub struct ObjectAccess<T> {
    pub object: Arc<T>,
    pub rights: Rights,
}

/// Линейный токен на зарезервированный слот: потребляется по значению ровно одним
/// из `commit_reserved`/`release_reservation`. Забытая резервация удерживает слот.
#[derive(Debug)]
pub struct HandleReservation {
    slot: u32,
    generation: NonZeroU16,
}

impl HandleReservation {
    pub fn handle_id(&self) -> HandleId {
        HandleId::pack(self.generation, self.slot)
    }
}

impl HandleTable {
    /// Стандартная ёмкость таблицы handle'ов одного процесса, обещанная syscall ABI.
    pub const DEFAULT_CAPACITY: u32 = syscall::HANDLE_TABLE_CAPACITY;

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

    /// Регистрирует handle, возвращая стабильный идентификатор. На `OutOfHandles` объект остаётся
    /// в [`RejectedInsert`].
    pub fn insert(&mut self, handle: Capability) -> Result<HandleId, RejectedInsert> {
        let Some((index, slot)) = self.claim_slot() else {
            return Err(RejectedInsert {
                error: IpcError::OutOfHandles,
                handle: Some(handle),
            });
        };
        let generation = slot.issue_generation();
        slot.state = SlotState::Occupied(handle);
        Ok(HandleId::pack(generation, index))
    }

    /// Резервирует слот; на исчерпании ёмкости - `OutOfHandles` без эффектов.
    pub fn reserve_slot(&mut self) -> Result<HandleReservation, IpcError> {
        let Some((index, slot)) = self.claim_slot() else {
            return Err(IpcError::OutOfHandles);
        };
        let generation = slot.issue_generation();
        slot.state = SlotState::Reserved;
        Ok(HandleReservation {
            slot: index,
            generation,
        })
    }

    // На отказе таблица не изменяется.
    fn claim_slot(&mut self) -> Option<(u32, &mut Slot)> {
        let index = if let Some(index) = self.pop_free_slot() {
            index
        } else {
            let index = self.slots.len() as u32;
            if index >= self.capacity {
                return None;
            }
            // Состояние - заглушка: вызывающий перезаписывает его сразу.
            self.slots.push(Slot::fresh(SlotState::Reserved));
            index
        };
        let slot = self.slots.get_mut(index as usize)?;
        slot.waiters.clear();
        Some((index, slot))
    }

    /// Вставляет `handle` в зарезервированный слот. Чужой живой слот не затирается: при
    /// несовпадении резервации `handle` уходит в `rejected`.
    // Токен по значению: его потребление и есть закрытие резервации.
    #[allow(clippy::needless_pass_by_value)]
    pub fn commit_reserved(
        &mut self,
        reservation: HandleReservation,
        handle: Capability,
        rejected: &mut DrainedHandles,
    ) -> HandleId {
        let id = reservation.handle_id();
        if let Some(slot) = self.reserved_slot_mut(&reservation) {
            slot.state = SlotState::Occupied(handle);
        } else {
            debug_assert!(false, "commit_reserved: slot is not the reserved one");
            rejected.push_handle(handle);
        }
        id
    }

    /// Возвращает зарезервированный слот в free-list и откатывает его поколение.
    #[allow(clippy::needless_pass_by_value)]
    pub fn release_reservation(&mut self, reservation: HandleReservation) {
        let prev_head = self.free_head;
        let index = reservation.slot;
        match self.reserved_slot_mut(&reservation) {
            Some(slot) => {
                slot.generation = slot.generation.saturating_sub(1);
                slot.state = SlotState::Free {
                    next_free: prev_head,
                };
                self.free_head = Some(index);
            }
            None => debug_assert!(false, "release_reservation: slot is not the reserved one"),
        }
    }

    fn reserved_slot_mut(&mut self, reservation: &HandleReservation) -> Option<&mut Slot> {
        let slot = self.slots.get_mut(reservation.slot as usize)?;
        let matches_reservation = matches!(slot.state, SlotState::Reserved)
            && slot.generation == reservation.generation.get();
        matches_reservation.then_some(slot)
    }

    /// Удаляет handle из таблицы, отдавая его содержимое в [`DrainedHandles`].
    pub fn remove(&mut self, id: HandleId) -> Result<DrainedHandles, IpcError> {
        let mut drained = DrainedHandles::empty();
        self.take_slot(id, &mut drained)?;
        Ok(drained)
    }

    fn take_slot(&mut self, id: HandleId, drained: &mut DrainedHandles) -> Result<(), IpcError> {
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

        drained.push_waiters(waiters);
        match taken {
            SlotState::Occupied(handle) => {
                drained.push_handle(handle);
                Ok(())
            }
            // Невозможно по проверке выше, но обходимся без unreachable!.
            _ => Err(IpcError::BadHandle),
        }
    }

    /// Изымает все занятые слоты и cancel-цели таблицы в [`DrainedHandles`].
    pub fn drain(&mut self) -> DrainedHandles {
        let mut drained = DrainedHandles::empty();
        self.drain_into(&mut drained);
        drained
    }

    fn drain_into(&mut self, drained: &mut DrainedHandles) {
        let mut free_head = self.free_head;

        for (idx, slot) in self.slots.iter_mut().enumerate() {
            drained.push_waiters(core::mem::take(&mut slot.waiters));
            if !matches!(slot.state, SlotState::Occupied(_)) {
                continue;
            }

            let taken = core::mem::replace(
                &mut slot.state,
                SlotState::Free {
                    next_free: free_head,
                },
            );
            free_head = Some(idx as u32);

            if let SlotState::Occupied(handle) = taken {
                drained.push_handle(handle);
            }
        }

        self.free_head = free_head;
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
    pub fn unregister_cancel(&mut self, id: HandleId, target: &dyn CancelTarget) {
        let Some(slot) = self.slots.get_mut(id.slot() as usize) else {
            return;
        };
        let target_ptr = core::ptr::from_ref(target).cast::<()>();
        if let Some(pos) = slot
            .waiters
            .iter()
            .position(|w| Arc::as_ptr(w).cast::<()>() == target_ptr)
        {
            slot.waiters.swap_remove(pos);
        }
    }

    /// Проверка прав без проверки типа.
    pub fn get(&self, id: HandleId, need: Rights) -> Result<&Capability, IpcError> {
        let handle = self.lookup(id)?;
        if !handle.rights().contains(need) {
            return Err(IpcError::AccessDenied);
        }
        Ok(handle)
    }

    /// Извлекает объект типа `T` с проверкой прав и типа. Соответствие типа
    /// варианту [`CapabilityTarget`] задаёт [`TargetObject`].
    pub fn get_object<T: TargetObject>(
        &self,
        id: HandleId,
        need: Rights,
    ) -> Result<Arc<T>, IpcError> {
        let handle = self.get(id, need)?;
        T::from_target(handle.target())
            .cloned()
            .ok_or(IpcError::WrongType)
    }

    /// Извлекает снимок port-capability для одной операции, с проверкой прав и типа: учёт стороны
    /// удерживается с этого момента и до снятия [`OperationHold`](super::OperationHold).
    pub fn get_port(&self, id: HandleId, need: Rights) -> Result<PortAccess, IpcError> {
        let handle = self.get(id, need)?;
        let port = Port::from_target(handle.target())
            .cloned()
            .ok_or(IpcError::WrongType)?;
        Ok(PortAccess {
            port,
            badge: handle.badge(),
            hold: handle.hold_for_operation(),
        })
    }

    /// Извлекает объект типа `T` вместе с полным набором `Rights` handle'а.
    pub fn get_object_with_rights<T: TargetObject>(
        &self,
        id: HandleId,
        need: Rights,
    ) -> Result<ObjectAccess<T>, IpcError> {
        let handle = self.get(id, need)?;
        let object = T::from_target(handle.target())
            .cloned()
            .ok_or(IpcError::WrongType)?;
        Ok(ObjectAccess {
            object,
            rights: handle.rights(),
        })
    }

    /// Доступ к capability target без проверки конкретного типа - для wait-пути,
    /// который применим к любому signalable (Signal/Process/...).
    pub fn clone_target(&self, id: HandleId, need: Rights) -> Result<CapabilityTarget, IpcError> {
        Ok(self.get(id, need)?.target().clone())
    }

    /// Атомарно проверяет, что все `ids` существуют, имеют `min_rights` и нет дубликатов, затем
    /// изымает их в `drained`. Капы для переноса отдаёт [`DrainedHandles::take_handles`].
    pub fn try_drain_for_transfer(
        &mut self,
        ids: &[HandleId],
        min_rights: Rights,
        drained: &mut DrainedHandles,
    ) -> Result<(), IpcError> {
        for (i, id) in ids.iter().enumerate() {
            if ids[..i].iter().any(|prev| prev == id) {
                return Err(IpcError::BadHandle);
            }
        }
        for id in ids {
            self.get(*id, min_rights)?;
        }
        // Валидация выше гарантирует, что take_slot не упадёт. На недоборе снятое остаётся в
        // `drained`.
        for id in ids {
            if self.take_slot(*id, drained).is_err() {
                return Err(IpcError::BadHandle);
            }
        }
        Ok(())
    }

    /// Создаёт новый handle на тот же capability target с подмножеством прав и (опционально) badge.
    /// Семантика значка - set-once, см. [`Capability::duplicate`].
    pub fn duplicate(
        &mut self,
        id: HandleId,
        new_rights: Rights,
        new_badge: u64,
    ) -> Result<HandleId, RejectedInsert> {
        let dup = {
            let handle = self.lookup(id).map_err(RejectedInsert::without_handle)?;
            handle
                .duplicate(new_rights, new_badge)
                .map_err(RejectedInsert::without_handle)?
        };
        self.insert(dup)
    }

    /// Регистрирует capability на объект `target`, выведенный из объекта
    /// записи `id`, производной от неё ([`Capability::derive`]). Происхождение
    /// проверяется здесь: объект, не выведенный из объекта записи, - `WrongType`.
    pub fn derive(
        &mut self,
        id: HandleId,
        target: impl Into<CapabilityTarget>,
    ) -> Result<HandleId, RejectedInsert> {
        let target = target.into();
        let derived = {
            let handle = self.lookup(id).map_err(RejectedInsert::without_handle)?;
            if !Self::is_target_derived_from(handle.target(), &target) {
                return Err(RejectedInsert::without_handle(IpcError::WrongType));
            }
            handle
                .derive(target)
                .map_err(RejectedInsert::without_handle)?
        };
        self.insert(derived)
    }

    // Производный Resource несёт источник в поле родителя, производный Memory-регион лежит
    // окном внутри источника с доступом не шире.
    fn is_target_derived_from(source: &CapabilityTarget, target: &CapabilityTarget) -> bool {
        match (source, target) {
            (CapabilityTarget::Resource(parent), CapabilityTarget::Resource(child)) => child
                .parent()
                .is_some_and(|origin| Arc::ptr_eq(origin, parent)),
            (CapabilityTarget::Memory(origin), CapabilityTarget::Memory(sub)) => {
                let (Some(origin_base), Some(sub_base)) =
                    (origin.physical_base(), sub.physical_base())
                else {
                    return false;
                };
                let origin_start = origin_base.as_usize();
                let sub_start = sub_base.as_usize();
                let (Some(origin_end), Some(sub_end)) = (
                    origin_start.checked_add(origin.size_bytes()),
                    sub_start.checked_add(sub.size_bytes()),
                ) else {
                    return false;
                };
                origin_start <= sub_start
                    && sub_end <= origin_end
                    && origin.access_mask().allows(sub.access_mask())
            }
            _ => false,
        }
    }

    fn lookup(&self, id: HandleId) -> Result<&Capability, IpcError> {
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
            let Some(slot) = self.slots.get_mut(idx as usize) else {
                self.free_head = None;
                return None;
            };

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
        let mut drained = DrainedHandles::empty();
        self.drain_into(&mut drained);
        drained.release();
    }
}

#[cfg(test)]
mod tests {
    use core::sync::atomic::{AtomicUsize, Ordering};

    use collections::{LockCell, MutexCell};
    use memory::MemoryRegion;

    use super::{
        super::{
            port::Port, process::ProcessObject, rev_node::RevocationHook, signal::Signal,
            target::CapabilityTarget, thread::ThreadObject, wait::CancelTarget,
        },
        *,
    };

    struct CountingHook(AtomicUsize);

    impl CountingHook {
        fn new() -> Arc<Self> {
            Arc::new(Self(AtomicUsize::new(0)))
        }

        fn count(&self) -> usize {
            self.0.load(Ordering::Acquire)
        }

        fn weak(self: &Arc<Self>) -> alloc::sync::Weak<dyn RevocationHook> {
            Arc::downgrade(self) as alloc::sync::Weak<dyn RevocationHook>
        }
    }

    impl RevocationHook for CountingHook {
        fn revoke(&self) {
            self.0.fetch_add(1, Ordering::AcqRel);
        }
    }

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

    // Захват table-лока из другого потока означает, что cancel сработал вне этого лока.
    struct LockProbeCancel {
        table: Arc<MutexCell<HandleTable>>,
        calls: AtomicUsize,
        acquisitions: AtomicUsize,
    }

    impl LockProbeCancel {
        fn new(table: &Arc<MutexCell<HandleTable>>) -> Arc<Self> {
            Arc::new(Self {
                table: table.clone(),
                calls: AtomicUsize::new(0),
                acquisitions: AtomicUsize::new(0),
            })
        }

        fn calls(&self) -> usize {
            self.calls.load(Ordering::Acquire)
        }

        fn acquisitions(&self) -> usize {
            self.acquisitions.load(Ordering::Acquire)
        }
    }

    impl CancelTarget for LockProbeCancel {
        fn cancel(&self) {
            self.calls.fetch_add(1, Ordering::AcqRel);
            let table = self.table.clone();
            let (sender, receiver) = std::sync::mpsc::channel();
            std::thread::spawn(move || {
                let _ = sender.send(table.with_lock(|tbl| tbl.live_count()));
            });
            if receiver
                .recv_timeout(core::time::Duration::from_secs(5))
                .is_ok()
            {
                self.acquisitions.fetch_add(1, Ordering::AcqRel);
            }
        }
    }

    fn make_handle(obj: CapabilityTarget, rights: Rights) -> Capability {
        Capability::new(obj, rights)
    }

    fn close(table: &mut HandleTable, id: HandleId) -> Result<(), IpcError> {
        table.remove(id).map(DrainedHandles::release)
    }

    fn drain_for_transfer(
        table: &mut HandleTable,
        ids: &[HandleId],
        min_rights: Rights,
    ) -> Result<Vec<Capability>, IpcError> {
        let mut drained = DrainedHandles::empty();
        match table.try_drain_for_transfer(ids, min_rights, &mut drained) {
            Ok(()) => {
                let handles = drained.take_handles();
                drained.release();
                Ok(handles)
            }
            Err(e) => {
                drained.release();
                Err(e)
            }
        }
    }

    fn commit(
        table: &mut HandleTable,
        reservation: HandleReservation,
        handle: Capability,
    ) -> HandleId {
        let mut rejected = DrainedHandles::empty();
        let id = table.commit_reserved(reservation, handle, &mut rejected);
        assert!(rejected.is_empty(), "commit must not reject its own slot");
        id
    }

    fn signal_handle(rights: Rights) -> Capability {
        make_handle(CapabilityTarget::Signal(Signal::new()), rights)
    }

    fn port_handle(rights: Rights) -> Capability {
        make_handle(CapabilityTarget::Port(Port::new()), rights)
    }

    fn process_handle(rights: Rights) -> Capability {
        make_handle(CapabilityTarget::Process(ProcessObject::new()), rights)
    }

    fn thread_handle(rights: Rights) -> Capability {
        make_handle(CapabilityTarget::Thread(ThreadObject::new()), rights)
    }

    #[test]
    fn insert_and_get_round_trip() {
        let mut table = HandleTable::new();
        let port = Port::new();
        let h = make_handle(
            CapabilityTarget::Port(port.clone()),
            Rights::READ | Rights::WRITE,
        );

        let id = table.insert(h).unwrap();
        let got = table.get(id, Rights::READ).expect("get must succeed");
        let CapabilityTarget::Port(got_port) = got.target() else {
            panic!("inserted handle must keep object type");
        };
        assert!(Arc::ptr_eq(got_port, &port));
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

        assert!(table.get_object::<Signal>(signal_id, Rights::READ).is_ok());
        assert!(table.get_port(ep_id, Rights::READ).is_ok());

        assert!(matches!(
            table.get_port(signal_id, Rights::READ),
            Err(IpcError::WrongType)
        ));
        assert!(matches!(
            table.get_object::<Signal>(ep_id, Rights::READ),
            Err(IpcError::WrongType)
        ));
        assert!(matches!(
            table.get_port(proc_id, Rights::READ),
            Err(IpcError::WrongType)
        ));
        assert!(matches!(
            table.get_object::<Signal>(thread_id, Rights::READ),
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

        assert!(
            table
                .get_object::<ProcessObject>(proc_id, Rights::READ)
                .is_ok()
        );
        assert!(matches!(
            table.get_object::<ProcessObject>(signal_id, Rights::READ),
            Err(IpcError::WrongType)
        ));
        assert!(matches!(
            table.get_object::<ProcessObject>(ep_id, Rights::READ),
            Err(IpcError::WrongType)
        ));
        assert!(matches!(
            table.get_object::<ProcessObject>(thread_id, Rights::READ),
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

        assert!(
            table
                .get_object::<ThreadObject>(thread_id, Rights::READ)
                .is_ok()
        );
        assert!(matches!(
            table.get_object::<ThreadObject>(signal_id, Rights::READ),
            Err(IpcError::WrongType)
        ));
        assert!(matches!(
            table.get_object::<ThreadObject>(ep_id, Rights::READ),
            Err(IpcError::WrongType)
        ));
        assert!(matches!(
            table.get_object::<ThreadObject>(proc_id, Rights::READ),
            Err(IpcError::WrongType)
        ));
    }

    #[test]
    fn get_process_checks_rights() {
        let mut table = HandleTable::new();
        let id = table.insert(process_handle(Rights::READ)).unwrap();
        assert!(matches!(
            table.get_object::<ProcessObject>(id, Rights::WRITE),
            Err(IpcError::AccessDenied)
        ));
        assert!(table.get_object::<ProcessObject>(id, Rights::READ).is_ok());
    }

    #[test]
    fn get_thread_checks_rights() {
        let mut table = HandleTable::new();
        let id = table.insert(thread_handle(Rights::READ)).unwrap();
        assert!(matches!(
            table.get_object::<ThreadObject>(id, Rights::WRITE),
            Err(IpcError::AccessDenied)
        ));
        assert!(table.get_object::<ThreadObject>(id, Rights::READ).is_ok());
    }

    #[test]
    fn get_memory_type_checks() {
        use core::num::NonZeroUsize;

        use memory::AccessMask;

        let region = Arc::new(MemoryRegion::create_physical_device(
            memory::physical_address::PageAlignedAddress::from_usize(0x4000_0000).unwrap(),
            NonZeroUsize::new(4096).unwrap(),
            AccessMask::R,
        ));
        let mut table = HandleTable::new();
        let mem_rights = Rights::WRITE | Rights::READ;
        let mem_id = table
            .insert(make_handle(CapabilityTarget::Memory(region), mem_rights))
            .unwrap();
        let signal_id = table.insert(signal_handle(Rights::READ)).unwrap();
        let ep_id = table.insert(port_handle(Rights::READ)).unwrap();

        assert!(
            table
                .get_object::<MemoryRegion>(mem_id, Rights::WRITE)
                .is_ok()
        );
        assert!(matches!(
            table.get_object::<MemoryRegion>(signal_id, Rights::READ),
            Err(IpcError::WrongType)
        ));
        assert!(matches!(
            table.get_object::<MemoryRegion>(ep_id, Rights::READ),
            Err(IpcError::WrongType)
        ));
        assert!(matches!(
            table.get_port(mem_id, Rights::READ),
            Err(IpcError::WrongType)
        ));
        assert!(matches!(
            table.get_object::<Signal>(mem_id, Rights::READ),
            Err(IpcError::WrongType)
        ));
    }

    fn memory_handle(rights: Rights) -> Capability {
        make_handle(CapabilityTarget::Memory(sub_region()), rights)
    }

    #[test]
    fn derived_target_is_revoked_with_its_source() {
        let mut table = HandleTable::new();
        let source = table
            .insert(memory_handle(Rights::DUPLICATE | Rights::READ))
            .unwrap();
        let sub = table.derive(source, sub_region()).unwrap();
        assert!(table.get_object::<MemoryRegion>(sub, Rights::READ).is_ok());

        close(&mut table, source).unwrap();

        assert!(matches!(
            table.get_object::<MemoryRegion>(sub, Rights::READ),
            Err(IpcError::Revoked)
        ));
    }

    #[test]
    fn derive_requires_duplicate_right() {
        let mut table = HandleTable::new();
        let source = table.insert(memory_handle(Rights::READ)).unwrap();
        assert_eq!(
            table.derive(source, sub_region()).unwrap_err().error(),
            IpcError::AccessDenied
        );
    }

    #[test]
    fn derive_rejects_a_target_outside_the_source() {
        use core::num::NonZeroUsize;

        use memory::AccessMask;

        let mut table = HandleTable::new();
        let source = table
            .insert(memory_handle(Rights::DUPLICATE | Rights::READ))
            .unwrap();

        // Окно вне источника: происхождение от объекта записи не подтверждено.
        let foreign = Arc::new(MemoryRegion::create_physical_device(
            memory::physical_address::PageAlignedAddress::from_usize(0x8000_0000).unwrap(),
            NonZeroUsize::new(4096).unwrap(),
            AccessMask::R,
        ));
        assert_eq!(
            table.derive(source, foreign).unwrap_err().error(),
            IpcError::WrongType
        );

        // Доступ шире источника не проходит и при вложенном окне.
        let escalated = Arc::new(MemoryRegion::create_physical_device(
            memory::physical_address::PageAlignedAddress::from_usize(0x4000_0000).unwrap(),
            NonZeroUsize::new(4096).unwrap(),
            AccessMask::RW,
        ));
        assert_eq!(
            table.derive(source, escalated).unwrap_err().error(),
            IpcError::WrongType
        );
    }

    #[test]
    fn derived_rights_are_narrowed_by_source() {
        let mut table = HandleTable::new();
        // Источник без TRANSFER: производная не получает его из стартового набора.
        let source = table
            .insert(memory_handle(Rights::DUPLICATE | Rights::READ))
            .unwrap();
        let sub = table.derive(source, sub_region()).unwrap();

        assert!(table.get_object::<MemoryRegion>(sub, Rights::READ).is_ok());
        assert!(matches!(
            table.get_object::<MemoryRegion>(sub, Rights::TRANSFER),
            Err(IpcError::AccessDenied)
        ));
    }

    fn sub_region() -> Arc<MemoryRegion> {
        use core::num::NonZeroUsize;

        use memory::AccessMask;

        Arc::new(MemoryRegion::create_physical_device(
            memory::physical_address::PageAlignedAddress::from_usize(0x4000_0000).unwrap(),
            NonZeroUsize::new(4096).unwrap(),
            AccessMask::R,
        ))
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
            .insert(make_handle(
                CapabilityTarget::Resource(resource),
                res_rights,
            ))
            .unwrap();
        let signal_id = table.insert(signal_handle(Rights::READ)).unwrap();
        let ep_id = table.insert(port_handle(Rights::READ)).unwrap();

        assert!(table.get_object::<Resource>(res_id, Rights::WRITE).is_ok());
        assert_eq!(
            table
                .get_object::<Resource>(res_id, Rights::WRITE | Rights::EXECUTE)
                .unwrap_err(),
            IpcError::AccessDenied
        );
        assert!(matches!(
            table.get_object::<Resource>(signal_id, Rights::READ),
            Err(IpcError::WrongType)
        ));
        assert!(matches!(
            table.get_object::<Resource>(ep_id, Rights::READ),
            Err(IpcError::WrongType)
        ));
        assert!(matches!(
            table.get_port(res_id, Rights::READ),
            Err(IpcError::WrongType)
        ));
        assert!(matches!(
            table.get_object::<Signal>(res_id, Rights::READ),
            Err(IpcError::WrongType)
        ));
    }

    #[test]
    fn remove_then_get_returns_bad_handle() {
        let mut table = HandleTable::new();
        let id = table.insert(port_handle(Rights::READ)).unwrap();

        close(&mut table, id).unwrap();
        assert_eq!(
            table.get(id, Rights::READ).unwrap_err(),
            IpcError::BadHandle
        );
    }

    #[test]
    fn double_close_returns_bad_handle() {
        let mut table = HandleTable::new();
        let id = table.insert(signal_handle(Rights::READ)).unwrap();

        close(&mut table, id).unwrap();
        assert_eq!(close(&mut table, id).unwrap_err(), IpcError::BadHandle);
    }

    #[test]
    fn out_of_range_slot_returns_bad_handle() {
        let table = HandleTable::with_capacity(4);
        let bogus = HandleId::pack(NonZeroU16::MIN, 999);
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
                .unwrap_err()
                .error(),
            IpcError::AccessDenied
        );
    }

    #[test]
    fn duplicate_requires_duplicate_right() {
        let mut table = HandleTable::new();
        let id = table.insert(port_handle(Rights::READ)).unwrap();

        assert_eq!(
            table.duplicate(id, Rights::READ, 0).unwrap_err().error(),
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
            table
                .duplicate(badged, Rights::READ, 0x22)
                .unwrap_err()
                .error(),
            IpcError::BadHandle
        );
    }

    #[test]
    fn slot_reuse_invalidates_old_id() {
        let mut table = HandleTable::with_capacity(2);
        let id1 = table.insert(signal_handle(Rights::READ)).unwrap();
        close(&mut table, id1).unwrap();

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
        assert_eq!(err.error(), IpcError::OutOfHandles);
    }

    #[test]
    fn rejected_insert_returns_the_object_instead_of_closing_it() {
        use alloc::sync::Arc;

        let mut table = HandleTable::with_capacity(1);
        table.insert(signal_handle(Rights::READ)).unwrap();

        let signal = Signal::new();
        let weak = Arc::downgrade(&signal);
        let handle = Capability::new(CapabilityTarget::Signal(signal), Rights::READ);
        let rejected = table.insert(handle).unwrap_err();
        assert_eq!(rejected.error(), IpcError::OutOfHandles);
        assert!(weak.upgrade().is_some());

        let mut other = HandleTable::with_capacity(1);
        other
            .insert(rejected.into_handle().expect("rejected object is returned"))
            .unwrap();
        assert_eq!(other.live_count(), 1);
    }

    #[test]
    fn generation_rollover_retires_slot() {
        let mut table = HandleTable::with_capacity(1);
        let max_gen = u32::from(HandleId::MAX_GENERATION);

        let mut id = table.insert(signal_handle(Rights::READ)).unwrap();
        assert_eq!(u32::from(id.generation()), 1);

        for expected_gen in 2..=max_gen {
            close(&mut table, id).unwrap();
            id = table.insert(signal_handle(Rights::READ)).unwrap();
            assert_eq!(u32::from(id.generation()), expected_gen);
        }

        close(&mut table, id).unwrap();
        assert_eq!(
            table
                .insert(signal_handle(Rights::READ))
                .unwrap_err()
                .error(),
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

        let drained = drain_for_transfer(&mut table, &[id1, id2], Rights::TRANSFER).unwrap();
        assert_eq!(drained.len(), 2);
        assert_eq!(table.live_count(), 0);
    }

    #[test]
    fn try_drain_for_transfer_empty_ok() {
        let mut table = HandleTable::new();
        let drained = drain_for_transfer(&mut table, &[], Rights::TRANSFER).unwrap();
        assert!(drained.is_empty());
        assert_eq!(table.live_count(), 0);
    }

    #[test]
    fn try_drain_for_transfer_missing_id_no_removal() {
        let mut table = HandleTable::new();
        let id = table
            .insert(signal_handle(Rights::READ | Rights::TRANSFER))
            .unwrap();
        let bogus = HandleId::pack(NonZeroU16::MIN, 999);

        let err = drain_for_transfer(&mut table, &[id, bogus], Rights::TRANSFER).unwrap_err();
        assert_eq!(err, IpcError::BadHandle);
        // Никаких изменений в таблице.
        assert_eq!(table.live_count(), 1);
        assert!(table.get(id, Rights::TRANSFER).is_ok());
    }

    #[test]
    fn try_drain_for_transfer_insufficient_rights_no_removal() {
        let mut table = HandleTable::new();
        let id = table.insert(signal_handle(Rights::READ)).unwrap();
        let err = drain_for_transfer(&mut table, &[id], Rights::TRANSFER).unwrap_err();
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
        let err = drain_for_transfer(&mut table, &[id, id], Rights::TRANSFER).unwrap_err();
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
        let bogus = HandleId::pack(NonZeroU16::MIN, 999);

        let err = drain_for_transfer(&mut table, &[id1, id2, bogus], Rights::TRANSFER).unwrap_err();
        assert_eq!(err, IpcError::BadHandle);
        assert_eq!(table.live_count(), 2);
    }

    #[test]
    fn tables_are_independent() {
        let mut a = HandleTable::new();
        let mut b = HandleTable::new();

        let id_a = a.insert(port_handle(Rights::READ)).unwrap();
        let throwaway = b.insert(port_handle(Rights::WRITE)).unwrap();
        close(&mut b, throwaway).unwrap();
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

        let drained = table.remove(id).unwrap();
        assert_eq!(target.count(), 0);

        drained.release();
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

        close(&mut table, id).unwrap();
        assert_eq!(a.count(), 1);
        assert_eq!(b.count(), 1);
    }

    #[test]
    fn register_cancel_on_bad_handle_returns_err() {
        let mut table = HandleTable::new();
        let bogus = HandleId::pack(NonZeroU16::MIN, 999);
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
        close(&mut table, id).unwrap();
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
        table.unregister_cancel(id, dyn_target.as_ref());

        close(&mut table, id).unwrap();
        assert_eq!(target.count(), 0);
    }

    #[test]
    fn unregister_cancel_idempotent_and_tolerates_missing() {
        let mut table = HandleTable::new();
        let id = table.insert(signal_handle(Rights::READ)).unwrap();
        let target = CountingCancel::new();
        let dyn_target: Arc<dyn CancelTarget> = target.clone();
        table.register_cancel(id, dyn_target.clone()).unwrap();
        table.unregister_cancel(id, dyn_target.as_ref());
        table.unregister_cancel(id, dyn_target.as_ref());
        let bogus = HandleId::pack(NonZeroU16::new(99).unwrap(), 999);
        table.unregister_cancel(bogus, dyn_target.as_ref());
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

        let drained = drain_for_transfer(&mut table, &[id1, id2], Rights::TRANSFER).unwrap();
        assert_eq!(drained.len(), 2);
        assert_eq!(a.count(), 1);
        assert_eq!(b.count(), 1);
    }

    #[test]
    fn drain_empties_table_and_releases_capability_targets() {
        let mut table = HandleTable::new();
        let signal = Signal::new();
        let weak = Arc::downgrade(&signal);
        table
            .insert(make_handle(CapabilityTarget::Signal(signal), Rights::READ))
            .unwrap();
        table.insert(port_handle(Rights::READ)).unwrap();

        let drained = table.drain();
        assert_eq!(table.live_count(), 0);
        assert!(
            weak.upgrade().is_some(),
            "drained capabilities stay alive until release"
        );

        drained.release();
        assert!(weak.upgrade().is_none());
    }

    #[test]
    fn drain_fires_cancel_targets_only_on_release() {
        let mut table = HandleTable::new();
        let id = table.insert(signal_handle(Rights::READ)).unwrap();
        let target = CountingCancel::new();
        table
            .register_cancel(id, target.clone() as Arc<dyn CancelTarget>)
            .unwrap();

        let drained = table.drain();
        assert_eq!(target.count(), 0);

        drained.release();
        assert_eq!(target.count(), 1);
    }

    #[test]
    fn drain_revokes_derived_capability_in_other_table() {
        let mut grantor = HandleTable::new();
        let mut recipient = HandleTable::new();
        let root = grantor.insert(signal_handle(dup_rights())).unwrap();
        let derived = grantor.duplicate(root, dup_rights(), 0).unwrap();
        let recv_id = transfer(&mut grantor, &mut recipient, &[derived])[0];

        grantor.drain().release();

        assert_eq!(
            recipient.get(recv_id, Rights::READ).unwrap_err(),
            IpcError::Revoked
        );
    }

    #[test]
    fn drop_after_drain_has_nothing_left() {
        let mut table = HandleTable::new();
        let id = table.insert(signal_handle(Rights::READ)).unwrap();
        let target = CountingCancel::new();
        table
            .register_cancel(id, target.clone() as Arc<dyn CancelTarget>)
            .unwrap();

        table.drain().release();
        drop(table);

        assert_eq!(target.count(), 1);
    }

    #[test]
    fn insert_after_drain_reuses_freed_slot() {
        let mut table = HandleTable::with_capacity(1);
        let id = table.insert(signal_handle(Rights::READ)).unwrap();
        table.drain().release();

        let reused = table
            .insert(signal_handle(Rights::READ))
            .expect("drained slot must return to the free list");
        assert_eq!(id.slot(), reused.slot());
        assert_ne!(id.generation(), reused.generation());
    }

    #[test]
    fn slot_reuse_does_not_carry_over_old_waiters() {
        let mut table = HandleTable::with_capacity(1);
        let id1 = table.insert(signal_handle(Rights::READ)).unwrap();
        let target = CountingCancel::new();
        table
            .register_cancel(id1, target.clone() as Arc<dyn CancelTarget>)
            .unwrap();
        close(&mut table, id1).unwrap();
        assert_eq!(target.count(), 1);

        let id2 = table.insert(signal_handle(Rights::READ)).unwrap();
        assert_eq!(id1.slot(), id2.slot());
        close(&mut table, id2).unwrap();
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
        let id = commit(&mut table, reservation, signal_handle(Rights::READ));
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

        let _drained = drain_for_transfer(&mut table, &[id1], Rights::TRANSFER).expect("drain");

        let reservation = table
            .reserve_slot()
            .expect("post-drain reserve_slot must succeed");
        commit(&mut table, reservation, signal_handle(Rights::READ));
    }

    #[test]
    fn release_reservation_does_not_burn_generation() {
        let mut table = HandleTable::with_capacity(1);
        let seed_id = table.insert(signal_handle(Rights::READ)).expect("insert");
        let seed_gen = seed_id.generation();
        close(&mut table, seed_id).expect("remove");

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
        let drained = drain_for_transfer(src, ids, Rights::TRANSFER).expect("drain");
        reservations
            .into_iter()
            .zip(drained)
            .map(|(res, h)| commit(dst, res, h))
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

        close(&mut grantor, root).unwrap();
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

        close(&mut grantor, node_q).unwrap();

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

        close(&mut grantor, root).unwrap();

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
        close(&mut grantor, root).unwrap();

        assert_eq!(
            recipient
                .duplicate(recv_id, Rights::READ, 0)
                .unwrap_err()
                .error(),
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
        close(&mut grantor, root).unwrap();

        assert!(close(&mut recipient, recv_id).is_ok());
        assert_eq!(recipient.live_count(), 0);
    }

    #[test]
    fn ancestors_and_siblings_keep_access_after_descendant_close() {
        let mut table = HandleTable::new();
        let root = table.insert(signal_handle(dup_rights())).unwrap();
        let child_a = table.duplicate(root, dup_rights(), 0).unwrap();
        let child_b = table.duplicate(root, dup_rights(), 0).unwrap();

        close(&mut table, child_a).unwrap();

        assert!(table.get(root, Rights::READ).is_ok());
        assert!(table.get(child_b, Rights::READ).is_ok());
    }

    #[test]
    fn registered_hook_fires_on_close() {
        let mut table = HandleTable::new();
        let id = table.insert(signal_handle(dup_rights())).unwrap();
        let hook = CountingHook::new();
        table.register_revocation_hook(id, hook.weak()).unwrap();
        assert_eq!(hook.count(), 0);

        close(&mut table, id).unwrap();
        assert_eq!(hook.count(), 1);
    }

    #[test]
    fn registered_hook_fires_for_descendant_on_ancestor_close() {
        let mut grantor = HandleTable::new();
        let mut recipient = HandleTable::new();
        let root = grantor.insert(signal_handle(dup_rights())).unwrap();
        let derived = grantor.duplicate(root, dup_rights(), 0).unwrap();
        let recv_id = transfer(&mut grantor, &mut recipient, &[derived])[0];

        let hook = CountingHook::new();
        recipient
            .register_revocation_hook(recv_id, hook.weak())
            .unwrap();

        close(&mut grantor, root).unwrap();
        assert_eq!(hook.count(), 1);
    }

    #[test]
    fn remove_revokes_and_destroys_only_on_release() {
        let mut grantor = HandleTable::new();
        let mut recipient = HandleTable::new();
        let root = grantor.insert(signal_handle(dup_rights())).unwrap();
        let derived = grantor.duplicate(root, dup_rights(), 0).unwrap();
        let recv_id = transfer(&mut grantor, &mut recipient, &[derived])[0];
        let hook = CountingHook::new();
        recipient
            .register_revocation_hook(recv_id, hook.weak())
            .unwrap();

        let drained = grantor.remove(root).unwrap();
        assert_eq!(hook.count(), 0, "отзыв не идёт из-под table-лока");
        assert!(
            recipient.get(recv_id, Rights::READ).is_ok(),
            "изъятая капа жива, пока её не разрушил release",
        );

        drained.release();
        assert_eq!(hook.count(), 1);
        assert_eq!(
            recipient.get(recv_id, Rights::READ).unwrap_err(),
            IpcError::Revoked
        );
    }

    #[test]
    fn drain_revokes_only_on_release() {
        let mut grantor = HandleTable::new();
        let mut recipient = HandleTable::new();
        let root = grantor.insert(signal_handle(dup_rights())).unwrap();
        let derived = grantor.duplicate(root, dup_rights(), 0).unwrap();
        let recv_id = transfer(&mut grantor, &mut recipient, &[derived])[0];
        let hook = CountingHook::new();
        recipient
            .register_revocation_hook(recv_id, hook.weak())
            .unwrap();

        let drained = grantor.drain();
        assert_eq!(hook.count(), 0, "отзыв не идёт из-под table-лока");

        drained.release();
        assert_eq!(hook.count(), 1);
    }

    #[test]
    fn remove_cancels_outside_the_table_lock() {
        let table = Arc::new(MutexCell::new(HandleTable::new()));
        let id = table
            .with_lock(|tbl| tbl.insert(signal_handle(Rights::READ)))
            .unwrap();
        let probe = LockProbeCancel::new(&table);
        table
            .with_lock(|tbl| tbl.register_cancel(id, probe.clone() as Arc<dyn CancelTarget>))
            .unwrap();

        table.with_lock(|tbl| tbl.remove(id)).unwrap().release();

        assert_eq!(probe.calls(), 1);
        assert_eq!(probe.acquisitions(), 1, "cancel обязан идти вне table-лока");
    }

    #[test]
    fn try_drain_for_transfer_cancels_outside_the_table_lock() {
        let table = Arc::new(MutexCell::new(HandleTable::new()));
        let id = table
            .with_lock(|tbl| tbl.insert(signal_handle(Rights::READ | Rights::TRANSFER)))
            .unwrap();
        let probe = LockProbeCancel::new(&table);
        table
            .with_lock(|tbl| tbl.register_cancel(id, probe.clone() as Arc<dyn CancelTarget>))
            .unwrap();

        let mut drained = DrainedHandles::empty();
        table
            .with_lock(|tbl| tbl.try_drain_for_transfer(&[id], Rights::TRANSFER, &mut drained))
            .unwrap();
        assert!(!drained.is_empty());
        drained.release();

        assert_eq!(probe.calls(), 1);
        assert_eq!(probe.acquisitions(), 1, "cancel обязан идти вне table-лока");
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
