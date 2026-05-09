use alloc::sync::Arc;

use super::{channel::ChannelEndpoint, event::Event, koid::Koid, wait::SignalState};

/// Kernel-объект (KO) - единица, к которой ядро выдаёт права.
///
/// Любой ресурс, доступ к которому процесс получает через capability -
/// канал, событие; в перспективе поток, MMIO-регион, IRQ.
/// Один KO может быть доступен нескольким процессам через разные
/// handle'ы с разными правами.
///
/// Закрытый список вариантов даёт compile-time exhaustiveness:
/// добавление нового KO заставит компилятор показать все match'и,
/// требующие обновления.
pub enum KObject {
    Channel(Arc<ChannelEndpoint>),
    Event(Arc<Event>),
}

impl KObject {
    /// Численный 8-битный тег типа. Стабилен в пределах сессии,
    /// упаковывается в [`Koid`] и используется как wire-format
    /// при будущей сериализации в syscall ABI.
    pub const fn type_tag(&self) -> u8 {
        match self {
            Self::Channel(_) => 1,
            Self::Event(_) => 2,
        }
    }

    /// Уникальный идентификатор объекта (type-tag + heap-адрес).
    pub fn koid(&self) -> Koid {
        match self {
            Self::Channel(c) => Koid::from_parts(self.type_tag(), Arc::as_ptr(c) as u64),
            Self::Event(e) => Koid::from_parts(self.type_tag(), Arc::as_ptr(e) as u64),
        }
    }

    /// Сигнальное состояние, если KO сигнализуем.
    /// Сейчас оба варианта сигнализуемы; при добавлении
    /// несигналуемых KO (Process, Thread) match станет частичным
    /// и компилятор подсветит все callsites.
    pub fn signals(&self) -> Option<&SignalState> {
        match self {
            Self::Channel(c) => Some(c.signals()),
            Self::Event(e) => Some(e.signals()),
        }
    }
}

impl Clone for KObject {
    fn clone(&self) -> Self {
        match self {
            Self::Channel(c) => Self::Channel(c.clone()),
            Self::Event(e) => Self::Event(e.clone()),
        }
    }
}

impl core::fmt::Debug for KObject {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        let name = match self {
            Self::Channel(_) => "Channel",
            Self::Event(_) => "Event",
        };
        f.debug_struct(name).field("koid", &self.koid()).finish()
    }
}
