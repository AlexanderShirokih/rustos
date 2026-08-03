//! `Process` capability target: lifecycle-объект процесса.

use alloc::sync::Arc;

use collections::{LockCell, MutexCell};
use syscall::Rights;

use super::{resource::Resource, signal::Signal, termination::TerminationState};

/// Метеринг-ресурс процесса вместе с правами, с которыми его передали.
#[derive(Clone)]
pub struct MeteringGrant {
    pub resource: Arc<Resource>,
    /// Права капы, которую `ProcessResourceSelf` кладёт в таблицу процесса: не шире прав
    /// хэндла, которым ресурс отдали при старте.
    pub rights: Rights,
}

pub struct ProcessObject {
    inner: TerminationState,
    metering: MutexCell<Option<MeteringGrant>>,
}

impl ProcessObject {
    /// Создаёт новый `ProcessObject`.
    pub fn new() -> Arc<Self> {
        Arc::new(Self {
            inner: TerminationState::new(),
            metering: MutexCell::new(None),
        })
    }

    /// Задаёт метеринг текущему процессу.
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

    /// Отвязывает metering-грант от объекта процесса.
    pub fn clear_metering(&self) {
        self.metering.with_lock(Option::take);
    }

    /// Публикует `code` и помечает процесс завершённым. Повторный вызов - no-op.
    pub fn signal_terminated(&self, code: i32) {
        self.inner.signal_terminated(code);
    }

    /// Помечает процесс завершённым и публикует `code`. Наблюдателей не будит.
    pub fn mark_terminated(&self, code: i32) {
        self.inner.mark_terminated(code);
    }

    /// Будит наблюдателей завершения процесса.
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

    /// Ленивый [`Signal`] терминации (бит `SIGNALED`). Материализуется
    /// при первом вызове и пре-сигналится, если процесс уже завершён.
    pub fn termination_signal(&self) -> Arc<Signal> {
        self.inner.termination_signal()
    }
}

#[cfg(test)]
mod tests {
    use core::num::NonZeroUsize;

    use memory::{AccessMask, physical_address::PageAlignedAddress};

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
    fn clearing_metering_preserves_termination_state() {
        let parent = Resource::new(
            PageAlignedAddress::from_usize(0x4000_0000).unwrap(),
            NonZeroUsize::new(0x1000).unwrap(),
            AccessMask::RW,
            4,
        );
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
