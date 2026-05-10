use alloc::sync::Arc;

use super::{
    channel::Channel, event::Event, koid::Koid, process::ProcessObject, thread::ThreadObject,
    wait::SignalState,
};

/// Kernel-объект (KO) - единица, к которой ядро выдаёт права.
///
/// Любой ресурс, доступ к которому процесс получает через capability -
/// канал, событие, lifecycle процесса/потока; в перспективе MMIO-регион,
/// IRQ. Один KO может быть доступен нескольким процессам через разные
/// handle'ы с разными правами.
///
/// Закрытый список вариантов даёт compile-time exhaustiveness:
/// добавление нового KO заставит компилятор показать все match'и,
/// требующие обновления.
pub enum KObject {
    Channel(Arc<Channel>),
    Event(Arc<Event>),
    Process(Arc<ProcessObject>),
    Thread(Arc<ThreadObject>),
}

impl KObject {
    /// Численный 8-битный тег типа. Стабилен в пределах сессии,
    /// упаковывается в [`Koid`] и используется как wire-format
    /// при будущей сериализации в syscall ABI.
    pub const fn type_tag(&self) -> u8 {
        match self {
            Self::Channel(_) => 1,
            Self::Event(_) => 2,
            Self::Process(_) => 3,
            Self::Thread(_) => 4,
        }
    }

    /// Уникальный идентификатор объекта (type-tag + heap-адрес).
    pub fn koid(&self) -> Koid {
        match self {
            Self::Channel(c) => Koid::from_parts(self.type_tag(), Arc::as_ptr(c) as u64),
            Self::Event(e) => Koid::from_parts(self.type_tag(), Arc::as_ptr(e) as u64),
            Self::Process(p) => Koid::from_parts(self.type_tag(), Arc::as_ptr(p) as u64),
            Self::Thread(t) => Koid::from_parts(self.type_tag(), Arc::as_ptr(t) as u64),
        }
    }

    /// Сигнальное состояние, если KO сигнализуем.
    /// Сейчас все варианты сигнализуемы; при добавлении несигналуемых KO
    /// match станет частичным и компилятор подсветит все callsites.
    pub fn signals(&self) -> Option<&SignalState> {
        match self {
            Self::Channel(c) => Some(c.signals()),
            Self::Event(e) => Some(e.signals()),
            Self::Process(p) => Some(p.signals()),
            Self::Thread(t) => Some(t.signals()),
        }
    }
}

impl Clone for KObject {
    fn clone(&self) -> Self {
        match self {
            Self::Channel(c) => Self::Channel(c.clone()),
            Self::Event(e) => Self::Event(e.clone()),
            Self::Process(p) => Self::Process(p.clone()),
            Self::Thread(t) => Self::Thread(t.clone()),
        }
    }
}

impl core::fmt::Debug for KObject {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        let name = match self {
            Self::Channel(_) => "Channel",
            Self::Event(_) => "Event",
            Self::Process(_) => "Process",
            Self::Thread(_) => "Thread",
        };
        f.debug_struct(name).field("koid", &self.koid()).finish()
    }
}
