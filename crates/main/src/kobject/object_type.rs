/// Тип kernel-объекта.
///
/// Используется для быстрого type-check на границе IPC.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
#[repr(u8)]
pub enum ObjectType {
    Process = 1,
    Thread = 2,
    Channel = 3,
    Event = 4,
    Timer = 5,
}
