//! `Event` KO: базовый сигнальный примитив с битами 0..=31.
//!
//! Используется для уведомлений "точка-точка" и как ответный канал
//! в RPC-паттерне "request -> handle на Event -> ждать SIGNALED".

use alloc::sync::Arc;

use super::{kernel_object::KernelObject, object_type::ObjectType, wait::SignalState};

/// Главный битовый сигнал "событие наступило".
pub const EVENT_SIGNALED: u32 = 1 << 0;

pub struct Event {
    signals: SignalState,
}

impl Event {
    /// Создаёт новый `Event` с очищенными сигналами.
    pub fn new() -> Arc<Self> {
        Arc::new(Self {
            signals: SignalState::new(0),
        })
    }

    /// Атомарно выставляет/снимает биты сигналов и будит ожидающих.
    pub fn signal(&self, set: u32, clear: u32) {
        self.signals.signal(set, clear);
    }

    /// Текущий снимок сигналов (без блокировок).
    pub fn peek(&self) -> u32 {
        self.signals.peek()
    }

    /// Прямой доступ к [`SignalState`] для интеграции с `object_wait_one`.
    pub fn signal_state(&self) -> &SignalState {
        &self.signals
    }
}

impl KernelObject for Event {
    fn object_type(&self) -> ObjectType {
        ObjectType::Event
    }

    fn signal_state(&self) -> Option<&SignalState> {
        Some(&self.signals)
    }

    fn as_event(&self) -> Option<&Event> {
        Some(self)
    }
}

#[cfg(test)]
mod tests {
    use super::{super::wait::MockWaker, *};

    #[test]
    fn signal_wakes_observer() {
        let event = Event::new();
        let w = MockWaker::new();
        event.signals.register_waiter(EVENT_SIGNALED, w.clone());

        assert!(!w.was_woken());
        event.signal(EVENT_SIGNALED, 0);
        assert!(w.was_woken());
        assert_eq!(w.observed() & EVENT_SIGNALED, EVENT_SIGNALED);
    }

    #[test]
    fn peek_returns_current_state() {
        let event = Event::new();
        assert_eq!(event.peek(), 0);
        event.signal(EVENT_SIGNALED, 0);
        assert_eq!(event.peek(), EVENT_SIGNALED);
        event.signal(0, EVENT_SIGNALED);
        assert_eq!(event.peek(), 0);
    }

    #[test]
    fn metadata_matches() {
        let event = Event::new();
        assert_eq!(event.object_type(), ObjectType::Event);
        // По кollocation Koid у разных Event разные.
        let other = Event::new();
        assert_ne!(event.koid(), other.koid());
    }

    #[test]
    fn as_event_downcast_works() {
        let event: Arc<dyn KernelObject> = Event::new();
        assert!(event.as_event().is_some());
        assert!(event.as_channel().is_none());
    }
}
