//! Значение декодированного кадра и владение его ресурсами.

use crate::wire::{Cap, HandleSlots};

/// Владелец доставленных ресурсов, независимый от следующей операции транспорта.
pub trait ReceivedResources {
    /// Владеющая capability платформы.
    type Handle;

    /// Передаёт capability одному потребителю.
    fn claim(&mut self, cap: Cap) -> Option<Self::Handle>;

    /// Закрывает ресурсы, которые не потреблены полностью успешным декодированием.
    fn retain_decoded(&mut self, slots: &HandleSlots);
}

/// Декодированное значение кадра вместе с владением его capability.
/// Невостребованные capability закрываются вместе с ответом.
#[derive(Debug)]
pub struct Response<T, R: ReceivedResources> {
    pub value: T,
    resources: R,
}

impl<T, R: ReceivedResources> Response<T, R> {
    #[doc(hidden)]
    pub fn new(value: T, resources: R) -> Self {
        Self { value, resources }
    }

    /// Переносит capability кадра во владеющий объект. Повторный запрос возвращает `None`.
    pub fn claim(&mut self, cap: Cap) -> Option<R::Handle> {
        self.resources.claim(cap)
    }
}

impl ReceivedResources for () {
    type Handle = core::convert::Infallible;

    fn claim(&mut self, _cap: Cap) -> Option<Self::Handle> {
        None
    }

    fn retain_decoded(&mut self, _slots: &HandleSlots) {}
}
