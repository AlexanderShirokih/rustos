//! Владение capability кадра: перенесённых ядром при доставке и отдаваемых при отправке.

use ipc::ReceivedResources;
use syscall::{Handle, IPC_BUFFER_MAX_CAPS, ObjectKind, kind};

use crate::IssuedHandle;

/// Сессия протокола `P`, принятая полем кадра. Порт сессии держит выданная ядром запись.
pub type ReceivedSession<P> = ipc::wire::Session<P, IssuedHandle<kind::Port>>;

/// Ресурсы кадра: значок, capability по слотам и канал ответа встречного вызова.
///
/// У принятого кадра невзятые capability закрываются вместе со значением. Исходящий кадр
/// получает владельцев при записи полей, и канал после успешной отправки снимает с них
/// закрытие.
#[derive(Debug, Default)]
pub struct Delivery {
    /// Значок порт-handle отправителя, `0` у незаклеймённого.
    pub badge: u64,
    reply: Option<IssuedHandle<kind::Reply>>,
    caps: [Option<IssuedHandle<kind::Any>>; IPC_BUFFER_MAX_CAPS],
}

impl Delivery {
    pub(crate) fn new(
        badge: u64,
        caps: [Option<Handle<kind::Any>>; IPC_BUFFER_MAX_CAPS],
        reply: Option<IssuedHandle<kind::Reply>>,
    ) -> Self {
        Self {
            badge,
            reply,
            caps: caps.map(|cap| cap.map(IssuedHandle::new)),
        }
    }

    /// Держат ли ресурсы capability в слотах.
    #[must_use]
    pub fn has_capabilities(&self) -> bool {
        self.caps.iter().any(Option::is_some)
    }

    pub(crate) fn numbers(&self) -> impl Iterator<Item = u32> + '_ {
        self.caps.iter().flatten().map(|cap| cap.as_raw().raw())
    }

    pub(crate) fn transferred(self) {
        for cap in self.caps.into_iter().flatten() {
            let _ = cap.leak();
        }
    }
}

impl ReceivedResources for Delivery {
    type Handle<K: ObjectKind> = IssuedHandle<K>;
    type Reply = IssuedHandle<kind::Reply>;

    fn take_handle<K: ObjectKind>(&mut self, index: u8) -> Option<IssuedHandle<K>> {
        let any = self.caps.get_mut(usize::from(index))?.take()?;
        Some(any.assume_kind())
    }

    fn put_handle<K: ObjectKind>(&mut self, handle: IssuedHandle<K>) -> Option<u8> {
        let index = self.caps.iter().position(Option::is_none)?;
        *self.caps.get_mut(index)? = Some(handle.erase());
        u8::try_from(index).ok()
    }

    fn reply_channel(handle: IssuedHandle<kind::Reply>) -> Option<IssuedHandle<kind::Reply>> {
        Some(handle)
    }

    fn take_reply(&mut self) -> Option<IssuedHandle<kind::Reply>> {
        self.reply.take()
    }
}
