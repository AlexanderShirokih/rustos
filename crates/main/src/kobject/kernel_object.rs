use super::{
    channel::ChannelEndpoint, event::Event, koid::Koid, object_type::ObjectType, wait::SignalState,
};

/// Kernel-объект (KO) - единица, к которой ядро выдаёт права.
///
/// Любой ресурс или примитив, доступ к которому процесс получает не напрямую, а через capability -
/// это KO: канал, событие, таймер, в перспективе поток, MMIO-регион, IRQ. Один KO может быть
/// доступен нескольким процессам через разные handle'ы с разными правами.
pub trait KernelObject: Send + Sync {
    fn koid(&self) -> Koid;
    fn object_type(&self) -> ObjectType;

    /// Сигнальное состояние объекта, если он сигнализуем (Event/Channel/Timer/...).
    /// Объекты без сигналов (Process, Thread в Phase 2) возвращают `None`.
    fn signal_state(&self) -> Option<&SignalState> {
        None
    }

    /// Type-safe "спуск" к конкретному KO. Реализуется только в самом
    /// `ChannelEndpoint`, чтобы внешний код не дёргал unsafe-downcast.
    fn as_channel(&self) -> Option<&ChannelEndpoint> {
        None
    }

    /// Аналогично [`Self::as_channel`], но для `Event`.
    fn as_event(&self) -> Option<&Event> {
        None
    }
}
