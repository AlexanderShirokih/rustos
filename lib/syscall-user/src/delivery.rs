//! Владение capability, перенесёнными ядром при доставке кадра.

use core::{fmt, mem::ManuallyDrop};

use ipc::{
    IntoWireValue, ReceivedResources,
    wire::{Cap, IpcError, MessageBuf},
};
use syscall::{Handle, IPC_BUFFER_MAX_CAPS, ObjectKind, kind};

use crate::handle_close;

/// Ресурсы доставленного кадра: значок, capability и канал ответа встречного вызова.
/// Невзятые закрываются вместе со значением.
#[derive(Debug, Default)]
pub struct Delivery {
    /// Значок порт-handle отправителя, `0` у незаклеймённого.
    pub badge: u64,
    reply: Option<ReceivedHandle<kind::Reply>>,
    caps: [Option<ReceivedHandle<kind::Any>>; IPC_BUFFER_MAX_CAPS],
}

/// Единственный владелец полученного хэндла до передачи объектной обёртке. Невзятый
/// [`ReceivedHandle`] закрывает свой handle при разрушении. Вид `K` приходит от поля
/// контракта, по которому capability извлечена.
pub struct ReceivedHandle<K: ObjectKind> {
    handle: Handle<K>,
}

impl Delivery {
    pub(crate) fn new(
        badge: u64,
        caps: [Option<Handle<kind::Any>>; IPC_BUFFER_MAX_CAPS],
        reply: Option<Handle<kind::Reply>>,
    ) -> Self {
        Self {
            badge,
            reply: reply.map(|handle| ReceivedHandle { handle }),
            caps: caps.map(|cap| cap.map(|handle| ReceivedHandle { handle })),
        }
    }
}

impl ReceivedResources for Delivery {
    type Handle<K: ObjectKind> = ReceivedHandle<K>;
    type Reply = ReceivedHandle<kind::Reply>;

    fn take_handle<K: ObjectKind>(&mut self, index: u8) -> Option<ReceivedHandle<K>> {
        let any = self.caps.get_mut(usize::from(index))?.take()?;
        Some(ReceivedHandle {
            handle: Handle::from_abi(any.into_raw().as_nonzero()),
        })
    }

    fn reply_channel(handle: ReceivedHandle<kind::Reply>) -> Option<ReceivedHandle<kind::Reply>> {
        Some(handle)
    }

    fn take_reply(&mut self) -> Option<ReceivedHandle<kind::Reply>> {
        self.reply.take()
    }
}

impl<K: ObjectKind> ReceivedHandle<K> {
    /// Передаёт владение в объектную обёртку, подавляя закрытие текущим владельцем. Закрыть
    /// такой handle - обязанность нового владельца.
    pub fn into_raw(self) -> Handle<K> {
        ManuallyDrop::new(self).handle
    }
}

impl<K: ObjectKind> IntoWireValue for ReceivedHandle<K> {
    type Read = Cap<K>;

    fn write_as_field(&self, buf: &mut MessageBuf, field_id: u8) -> Result<(), IpcError> {
        ipc::IntoWireValue::write_as_field(&Cap::from_handle(self.handle), buf, field_id)
    }

    fn relinquish(self) {
        let _ = self.into_raw();
    }
}

impl<K: ObjectKind> fmt::Debug for ReceivedHandle<K> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_tuple("ReceivedHandle").field(&self.handle).finish()
    }
}

impl<K: ObjectKind> Drop for ReceivedHandle<K> {
    fn drop(&mut self) {
        let _ = handle_close(self.handle);
    }
}
