//! Владение capability, перенесёнными ядром при доставке кадра.

use core::{fmt, mem::ManuallyDrop};

use ipc::{
    ReceivedResources,
    wire::{Cap, HandleSlots},
};
use syscall::{Handle, IPC_BUFFER_MAX_CAPS, ObjectKind, kind};

use crate::handle_close;

/// Ресурсы доставленного кадра. Незавершённым reply владеет транспорт.
#[derive(Debug, Default)]
pub struct Delivery {
    /// Значок порт-handle отправителя, `0` у незаклеймённого.
    pub badge: u64,
    /// Reply-handle кадра, `Some` только у встречного `call`. Значение здесь дублирует
    /// запись транспорта, и закрывает reply транспорт.
    pub reply: Option<Handle<kind::Reply>>,
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
            reply,
            caps: caps.map(|cap| cap.map(|handle| ReceivedHandle { handle })),
        }
    }
}

impl ReceivedResources for Delivery {
    type Handle<K: ObjectKind> = ReceivedHandle<K>;

    fn claim<K: ObjectKind>(&mut self, cap: Cap<K>) -> Option<ReceivedHandle<K>> {
        // Поиск по проверенному id и take выполняются до передачи владения, поэтому найденный
        // владелец отдаёт свою запись ровно один раз.
        let owned = self
            .caps
            .iter_mut()
            .find(|slot| {
                slot.as_ref()
                    .is_some_and(|owned| owned.handle.as_nonzero() == cap.raw())
            })
            .and_then(Option::take)?;
        let _ = ManuallyDrop::new(owned);
        Some(ReceivedHandle {
            handle: cap.as_handle(),
        })
    }

    fn retain_decoded(&mut self, slots: &HandleSlots) {
        for place in &mut self.caps {
            if place
                .as_ref()
                .is_some_and(|owned| !slots.decoded(owned.handle.raw()))
            {
                *place = None;
            }
        }
    }
}

impl<K: ObjectKind> ReceivedHandle<K> {
    /// Передаёт владение в объектную обёртку, подавляя закрытие текущим владельцем. Закрыть
    /// такой handle - обязанность нового владельца.
    pub fn into_raw(self) -> Handle<K> {
        ManuallyDrop::new(self).handle
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
