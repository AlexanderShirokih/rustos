//! Ресурсы кадра: его capability по слотам и канал ответа встречному вызову.

use syscall::{ObjectKind, kind};

/// Владелец ресурсов кадра: capability по слотам и канал ответа встречному вызову.
///
/// Ресурсы принятого кадра держат его capability до переноса в прочитанные значения. Разбор
/// поля-capability забирает её слот, и прочитанное значение владеет ею. Ресурсы исходящего
/// кадра получают владельцев при записи полей, и канал отдаёт их ядру. Capability, которых не
/// назвало ни одно поле, закрываются вместе с ресурсами.
pub trait ReceivedResources: Default {
    /// Владеющая capability платформы для вида объекта `K`.
    type Handle<K: ObjectKind>;

    /// Канал ответа вызывателю. Его кадр ответа держит ресурсы того же вида.
    type Reply: crate::ReplyChannel<Resources = Self>;

    /// Забирает capability слота `index`. Вид `K` задаёт поле контракта. `None` - слот пуст,
    /// уже взят либо отсутствует.
    fn take_handle<K: ObjectKind>(&mut self, index: u8) -> Option<Self::Handle<K>>;

    /// Кладёт capability `handle` в следующий свободный слот и возвращает индекс слота. `None`
    /// - нет свободного слота, и `handle` закрывается.
    fn put_handle<K: ObjectKind>(&mut self, handle: Self::Handle<K>) -> Option<u8>;

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

    fn put_handle<K: ObjectKind>(&mut self, handle: Self::Handle<K>) -> Option<u8> {
        match handle {}
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
    use core::{fmt, marker::PhantomData};
    use std::vec::Vec;

    use syscall::{ObjectKind, kind};

    use super::ReceivedResources;
    use crate::{
        IntoWireValue,
        wire::{Cap, IpcError, MessageBuf},
    };

    #[derive(Default)]
    pub(crate) struct TestFrame {
        slots: Vec<Option<u32>>,
    }

    pub(crate) struct TestCap<K: ObjectKind> {
        pub(crate) raw: u32,
        kind: PhantomData<fn() -> K>,
    }

    #[derive(Debug, PartialEq, Eq)]
    pub(crate) struct TestReply(pub(crate) u32);

    pub(crate) fn cap<K: ObjectKind>(raw: u32) -> TestCap<K> {
        TestCap {
            raw,
            kind: PhantomData,
        }
    }

    impl<K: ObjectKind> PartialEq for TestCap<K> {
        fn eq(&self, other: &Self) -> bool {
            self.raw == other.raw
        }
    }

    impl<K: ObjectKind> fmt::Debug for TestCap<K> {
        fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
            f.debug_tuple("TestCap").field(&self.raw).finish()
        }
    }

    impl TestFrame {
        pub(crate) fn numbers(&self) -> Vec<u32> {
            self.slots.iter().flatten().copied().collect()
        }
    }

    impl ReceivedResources for TestFrame {
        type Handle<K: ObjectKind> = TestCap<K>;
        type Reply = TestReply;

        fn take_handle<K: ObjectKind>(&mut self, index: u8) -> Option<TestCap<K>> {
            self.slots.get_mut(usize::from(index))?.take().map(cap)
        }

        fn put_handle<K: ObjectKind>(&mut self, handle: TestCap<K>) -> Option<u8> {
            let index = u8::try_from(self.slots.len()).ok()?;
            self.slots.push(Some(handle.raw));
            Some(index)
        }

        fn reply_channel(handle: TestCap<kind::Reply>) -> Option<TestReply> {
            Some(TestReply(handle.raw))
        }

        fn take_reply(&mut self) -> Option<TestReply> {
            None
        }
    }

    impl<K: ObjectKind> IntoWireValue<TestFrame> for TestCap<K> {
        type Read = Cap<K>;

        fn write_into(
            self,
            frame: &mut MessageBuf<TestFrame>,
            field_id: u8,
        ) -> Result<(), IpcError> {
            frame.write_handle(field_id, self)
        }
    }

    impl IntoWireValue<TestFrame> for TestReply {
        type Read = Cap<kind::Reply>;

        fn write_into(
            self,
            frame: &mut MessageBuf<TestFrame>,
            field_id: u8,
        ) -> Result<(), IpcError> {
            frame.write_handle(field_id, cap::<kind::Reply>(self.0))
        }
    }

    impl crate::ReplyChannel for TestReply {
        type Resources = TestFrame;

        fn send(
            self,
            frame: crate::Outgoing<'_, TestFrame>,
        ) -> Result<(), crate::Unsent<'_, TestFrame>> {
            Err((IpcError::PeerClosed, frame))
        }
    }
}
