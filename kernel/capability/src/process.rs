//! `Process` capability target: lifecycle-объект процесса.

use alloc::sync::Arc;
use core::sync::atomic::{AtomicU8, AtomicUsize, Ordering};

use kbase::{LockCell, MutexCell};
use syscall::Rights;

use super::{
    errors::IpcError, resource::Resource, rev_node::RevocationHook, runtime::installed_runtime,
    signal::Signal, target::HolderTicket, termination::TerminationState,
};

/// Метеринг-ресурс процесса вместе с правами, с которыми его передали.
#[derive(Clone)]
pub struct MeteringGrant {
    pub resource: Arc<Resource>,
    /// Права капы, которую `ProcessResourceSelf` кладёт в таблицу процесса: не шире прав
    /// хэндла, которым ресурс отдали при старте.
    pub rights: Rights,
}

const HOLDER_UNCOUNTED: u8 = 0;
const HOLDER_COUNTED: u8 = 1;
const HOLDER_RELEASED: u8 = 2;

struct ProcessHolder {
    process: Arc<ProcessObject>,
    state: AtomicU8,
}

impl HolderTicket for ProcessHolder {
    fn activate(&self) -> Result<(), IpcError> {
        if self.state.load(Ordering::SeqCst) != HOLDER_UNCOUNTED {
            return Err(IpcError::Revoked);
        }
        // Отзыв между проверкой и обменом уводит исходного держателя, поэтому откат ниже
        // идёт через `release_holder` и снимает процесс, если ушедший был последним.
        self.process.acquire_holder();
        if self
            .state
            .compare_exchange(
                HOLDER_UNCOUNTED,
                HOLDER_COUNTED,
                Ordering::SeqCst,
                Ordering::SeqCst,
            )
            .is_err()
        {
            self.process.release_holder();
            return Err(IpcError::Revoked);
        }
        Ok(())
    }

    fn release(&self) {
        if self.state.swap(HOLDER_RELEASED, Ordering::SeqCst) == HOLDER_COUNTED {
            self.process.release_holder();
        }
    }

    fn reacquire(&self) -> Option<Arc<dyn HolderTicket>> {
        None
    }
}

impl RevocationHook for ProcessHolder {
    fn revoke(&self) {
        self.release();
    }
}

pub struct ProcessObject {
    inner: TerminationState,
    exit_request: MutexCell<Option<i32>>,
    metering: MutexCell<Option<MeteringGrant>>,
    holders: AtomicUsize,
}

impl ProcessObject {
    pub fn new() -> Arc<Self> {
        Arc::new(Self {
            inner: TerminationState::new(),
            exit_request: MutexCell::new(None),
            metering: MutexCell::new(None),
            holders: AtomicUsize::new(0),
        })
    }

    pub(super) fn holder_ticket(self: &Arc<Self>, rights: Rights) -> Option<Arc<dyn HolderTicket>> {
        rights
            .contains(Rights::WRITE)
            .then(|| self.make_ticket(HOLDER_UNCOUNTED))
    }

    pub(super) fn counted_holder_ticket(
        self: &Arc<Self>,
        rights: Rights,
    ) -> Option<Arc<dyn HolderTicket>> {
        if !rights.contains(Rights::WRITE) {
            return None;
        }
        self.acquire_holder();
        Some(self.make_ticket(HOLDER_COUNTED))
    }

    fn make_ticket(self: &Arc<Self>, state: u8) -> Arc<dyn HolderTicket> {
        Arc::new(ProcessHolder {
            process: self.clone(),
            state: AtomicU8::new(state),
        })
    }

    fn acquire_holder(&self) {
        self.holders.fetch_add(1, Ordering::AcqRel);
    }

    fn release_holder(self: &Arc<Self>) {
        if self.holders.fetch_sub(1, Ordering::AcqRel) != 1 {
            return;
        }

        if self.exiting() {
            return;
        }

        if let Some(runtime) = installed_runtime() {
            runtime.release_process_holder(self);
        }
    }

    #[cfg(test)]
    pub(crate) fn holder_count(&self) -> usize {
        self.holders.load(Ordering::Acquire)
    }

    /// Задаёт метеринг-грант этого процесса. Грант ставится один раз. На занятом слоте вызов
    /// сохраняет прежний грант и разрушает `grant`.
    pub fn set_metering(&self, grant: MeteringGrant) {
        self.metering.with_lock(|slot| {
            if slot.is_none() {
                *slot = Some(grant);
            }
        });
    }

    pub fn metering(&self) -> Option<MeteringGrant> {
        self.metering.with_lock(|slot| slot.clone())
    }

    pub fn metering_resource(&self) -> Option<Arc<Resource>> {
        self.metering
            .with_lock(|slot| slot.as_ref().map(|grant| Arc::clone(&grant.resource)))
    }

    /// Отвязывает метеринг-грант от объекта процесса и открывает слот под новый грант. Когда на
    /// ресурс гранта не остаётся ссылок, невыбранный бюджет возвращается родительскому ресурсу.
    pub fn clear_metering(&self) {
        self.metering.with_lock(Option::take);
    }

    /// Публикует `code`, помечает процесс завершённым и будит наблюдателей. Повторный вызов -
    /// no-op, и код первой пометки сохраняется.
    pub fn signal_terminated(&self, code: i32) {
        self.inner.signal_terminated(code);
    }

    /// Помечает процесс завершённым и публикует `code`. Наблюдателей будит парный
    /// [`wake_terminated`](Self::wake_terminated), вызываемый уже вне scheduler-лока. Повторный
    /// вызов - no-op, и код первой пометки сохраняется.
    pub fn mark_terminated(&self, code: i32) {
        self.inner.mark_terminated(code);
    }

    /// Будит наблюдателей завершения процесса. Вызывается после
    /// [`mark_terminated`](Self::mark_terminated) и вне scheduler-лока. Пока bound-`Signal` не
    /// материализован обращением к [`termination_signal`](Self::termination_signal), вызов -
    /// no-op.
    pub fn wake_terminated(&self) {
        self.inner.wake_terminated();
    }

    /// Код завершения процесса. До завершения возвращает 0.
    pub fn exit_code(&self) -> i32 {
        self.inner.exit_code()
    }

    /// Завершён ли процесс.
    pub fn terminated(&self) -> bool {
        self.inner.terminated()
    }

    /// Ставит запрос снятия процесса и возвращает код, с которым процесс завершится. Код
    /// фиксирует первый запрос.
    pub fn request_exit(&self, code: i32) -> i32 {
        self.exit_request
            .with_lock(|request| *request.get_or_insert(code))
    }

    /// Код запроса снятия процесса. `None`, пока процесс не снимают.
    pub fn requested_exit_code(&self) -> Option<i32> {
        self.exit_request.with_lock(|request| *request)
    }

    /// Снимается ли процесс либо уже завершён. Новых потоков у такого процесса не будет.
    pub fn exiting(&self) -> bool {
        self.terminated() || self.requested_exit_code().is_some()
    }

    /// Ленивый [`Signal`] терминации (бит `SIGNALED`). Материализуется
    /// при первом вызове и пре-сигналится, если процесс уже завершён.
    pub fn termination_signal(&self) -> Arc<Signal> {
        self.inner.termination_signal()
    }
}

#[cfg(test)]
mod tests {
    use memory::AccessMask;

    use super::{super::signal::SIGNALED, *};

    #[test]
    fn process_delegates_lifecycle_to_termination_state() {
        let proc = ProcessObject::new();
        assert!(!proc.terminated());
        assert_eq!(proc.exit_code(), 0);

        proc.signal_terminated(42);
        assert!(proc.terminated());
        assert_eq!(proc.exit_code(), 42);
        assert_eq!(proc.termination_signal().peek() & SIGNALED, SIGNALED);
    }

    #[test]
    fn first_exit_request_fixes_the_code_before_termination() {
        let proc = ProcessObject::new();
        assert!(!proc.exiting());

        assert_eq!(proc.request_exit(7), 7);
        assert_eq!(proc.request_exit(9), 7);
        assert!(proc.exiting());
        assert!(!proc.terminated());
        assert_eq!(proc.termination_signal().peek() & SIGNALED, 0);
    }

    #[test]
    fn counted_ticket_holds_process_until_released() {
        let process = ProcessObject::new();
        let ticket = process
            .counted_holder_ticket(Rights::WRITE)
            .expect("write capability holds the process");
        assert_eq!(process.holder_count(), 1);

        ticket.release();
        assert_eq!(process.holder_count(), 0);
    }

    #[test]
    fn activated_ticket_holds_process_until_released() {
        let process = ProcessObject::new();
        let ticket = process
            .holder_ticket(Rights::WRITE)
            .expect("write capability holds the process");
        assert_eq!(process.holder_count(), 0);

        ticket.activate().expect("fresh ticket activates");
        assert_eq!(process.holder_count(), 1);

        ticket.release();
        assert_eq!(process.holder_count(), 0);
    }

    #[test]
    fn repeated_release_takes_the_count_once() {
        let process = ProcessObject::new();
        let first = process.counted_holder_ticket(Rights::WRITE).unwrap();
        let second = process.counted_holder_ticket(Rights::WRITE).unwrap();
        assert_eq!(process.holder_count(), 2);

        first.release();
        first.release();
        assert_eq!(process.holder_count(), 1);

        drop(second);
    }

    #[test]
    fn revoked_ticket_leaves_no_count_behind() {
        let process = ProcessObject::new();
        let ticket = process.holder_ticket(Rights::WRITE).unwrap();
        ticket.revoke();

        assert_eq!(ticket.activate(), Err(IpcError::Revoked));
        assert_eq!(process.holder_count(), 0);
    }

    #[test]
    fn capability_without_write_does_not_hold() {
        let process = ProcessObject::new();
        assert!(process.holder_ticket(Rights::READ).is_none());
        assert!(process.counted_holder_ticket(Rights::READ).is_none());
        assert_eq!(process.holder_count(), 0);
    }

    #[test]
    fn clearing_metering_preserves_termination_state() {
        let parent = Resource::new(AccessMask::RW, 4);
        let child = parent.split(4).unwrap();
        let process = ProcessObject::new();
        process.set_metering(MeteringGrant {
            resource: child,
            rights: Rights::READ | Rights::WRITE,
        });
        process.signal_terminated(42);

        process.clear_metering();

        assert_eq!(process.exit_code(), 42);
        assert!(process.terminated());
        assert_eq!(parent.remaining_budget(), 4);
    }
}
