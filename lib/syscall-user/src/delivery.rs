//! Владение capability, перенесёнными ядром при доставке кадра.

use core::mem::ManuallyDrop;

use ipc::{
    ReceivedResources,
    wire::{Cap, HandleSlots},
};
use syscall::{Handle, IPC_BUFFER_MAX_CAPS};

use crate::handle_close;

/// Ресурсы доставленного кадра. Незавершённым reply владеет транспорт.
#[derive(Debug, Default)]
pub struct Delivery {
    /// Значок порт-handle отправителя, `0` у незаклеймённого.
    pub badge: u64,
    /// Reply-handle кадра, `Some` только у встречного `call`. Значение здесь дублирует
    /// запись транспорта, и закрывает reply транспорт.
    pub reply: Option<Handle>,
    caps: [Option<ReceivedHandle>; IPC_BUFFER_MAX_CAPS],
}

/// Единственный владелец полученного хэндла до передачи объектной обёртке. Невзятый
/// [`ReceivedHandle`] закрывает свой handle при разрушении.
#[derive(Debug)]
pub struct ReceivedHandle {
    handle: Handle,
}

impl Delivery {
    pub(crate) fn new(
        badge: u64,
        caps: [Option<Handle>; IPC_BUFFER_MAX_CAPS],
        reply: Option<Handle>,
    ) -> Self {
        Self {
            badge,
            reply,
            caps: caps.map(|cap| cap.map(|handle| ReceivedHandle { handle })),
        }
    }
}

impl ReceivedResources for Delivery {
    type Handle = ReceivedHandle;

    fn claim(&mut self, cap: Cap) -> Option<ReceivedHandle> {
        self.caps
            .iter_mut()
            .find(|slot| {
                slot.as_ref()
                    .is_some_and(|owned| owned.handle.as_nonzero() == cap.raw())
            })
            .and_then(Option::take)
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

impl ReceivedHandle {
    /// Передаёт владение в объектную обёртку, подавляя закрытие текущим владельцем. Закрыть
    /// такой handle - обязанность нового владельца.
    pub fn into_raw(self) -> Handle {
        ManuallyDrop::new(self).handle
    }
}

impl Drop for ReceivedHandle {
    fn drop(&mut self) {
        let _ = handle_close(self.handle);
    }
}
