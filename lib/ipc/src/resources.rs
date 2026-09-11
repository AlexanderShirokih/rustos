//! Ресурсы принятого кадра: его capability и канал ответа встречному вызову.

use syscall::{ObjectKind, kind};

/// Владелец ресурсов принятого кадра до их переноса в прочитанные значения.
///
/// Разбор поля-capability забирает её слот, и прочитанное значение владеет ею. Capability,
/// которых не назвало ни одно поле, закрываются вместе с ресурсами.
pub trait ReceivedResources {
    /// Владеющая capability платформы для вида объекта `K`.
    type Handle<K: ObjectKind>;

    /// Канал ответа вызывателю.
    type Reply: crate::ReplyChannel;

    /// Забирает capability слота `index`. Вид `K` задаёт поле контракта. `None` - слот пуст,
    /// уже взят либо отсутствует.
    fn take_handle<K: ObjectKind>(&mut self, index: u8) -> Option<Self::Handle<K>>;

    /// Канал ответа из capability, пришедшей полем кадра. `None` - capability не даёт канала.
    fn reply_channel(handle: Self::Handle<kind::Reply>) -> Option<Self::Reply>;

    /// Забирает канал ответа на вызов этого кадра. `None`, когда кадр не ждёт ответа либо канал
    /// уже взят.
    fn take_reply(&mut self) -> Option<Self::Reply>;
}

/// Значение, которое получатель читает из записи типа `T` в кадре с ресурсами `R`.
pub type Received<T, R> = <T as crate::WireValue<'static, R>>::Owned;

impl ReceivedResources for () {
    type Handle<K: ObjectKind> = core::convert::Infallible;
    type Reply = core::convert::Infallible;

    fn take_handle<K: ObjectKind>(&mut self, _index: u8) -> Option<Self::Handle<K>> {
        None
    }

    fn reply_channel(handle: Self::Handle<kind::Reply>) -> Option<Self::Reply> {
        match handle {}
    }

    fn take_reply(&mut self) -> Option<Self::Reply> {
        None
    }
}

#[cfg(test)]
pub(crate) mod testing {
    use std::vec::Vec;

    use syscall::{Handle, ObjectKind, kind};

    use super::ReceivedResources;
    use crate::wire::{Cap, IpcError};

    pub(crate) struct TestFrame {
        slots: Vec<Option<u32>>,
    }

    #[derive(Debug, PartialEq, Eq)]
    pub(crate) struct TestReply(pub(crate) u32);

    impl TestFrame {
        pub(crate) fn of(handles: &[u32]) -> Self {
            Self {
                slots: handles.iter().copied().map(Some).collect(),
            }
        }
    }

    impl ReceivedResources for TestFrame {
        type Handle<K: ObjectKind> = Cap<K>;
        type Reply = TestReply;

        fn take_handle<K: ObjectKind>(&mut self, index: u8) -> Option<Cap<K>> {
            let id = self.slots.get_mut(usize::from(index))?.take()?;
            Some(Cap::from_handle(Handle::from_abi(
                core::num::NonZeroU32::new(id)?,
            )))
        }

        fn reply_channel(handle: Cap<kind::Reply>) -> Option<TestReply> {
            Some(TestReply(handle.raw().get()))
        }

        fn take_reply(&mut self) -> Option<TestReply> {
            None
        }
    }

    impl crate::ReplyChannel for TestReply {
        fn send(self, _bytes: &[u8], _handles: &[u32]) -> Result<(), IpcError> {
            Err(IpcError::PeerClosed)
        }
    }
}
