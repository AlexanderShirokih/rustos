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

impl ObjectType {
    pub fn from_u8(tag: u8) -> Option<Self> {
        match tag {
            1 => Some(Self::Process),
            2 => Some(Self::Thread),
            3 => Some(Self::Channel),
            4 => Some(Self::Event),
            5 => Some(Self::Timer),
            _ => None,
        }
    }
}
