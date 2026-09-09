//! Значение декодированного кадра и владение его ресурсами.

use syscall::ObjectKind;

use crate::wire::{Cap, HandleSlots};

/// Владелец доставленных ресурсов, независимый от следующей операции транспорта.
pub trait ReceivedResources {
    /// Владеющая capability платформы для вида объекта `K`.
    type Handle<K: ObjectKind>;

    /// Передаёт владение capability одному потребителю. Вид владельца задаёт вид поля.
    /// Отданная либо чужая `cap` - `None`.
    fn claim<K: ObjectKind>(&mut self, cap: Cap<K>) -> Option<Self::Handle<K>>;

    /// Закрывает доставленные capability, которых разбор кадра не потребил. Потреблённые
    /// слоты называет `slots`. Отказ разбора закрывает капы кадра целиком, минуя этот вызов.
    fn retain_decoded(&mut self, slots: &HandleSlots);
}

/// Декодированное значение кадра вместе с владением его capability.
/// Невостребованные capability закрываются вместе с ответом.
#[derive(Debug)]
pub struct Response<T, R: ReceivedResources> {
    /// Декодированное значение ответа.
    pub value: T,
    resources: R,
}

impl<T, R: ReceivedResources> Response<T, R> {
    #[doc(hidden)]
    pub fn new(value: T, resources: R) -> Self {
        Self { value, resources }
    }

    /// Переносит capability кадра во владеющий объект вида поля. Повторный запрос
    /// возвращает `None`.
    pub fn claim<K: ObjectKind>(&mut self, cap: Cap<K>) -> Option<R::Handle<K>> {
        self.resources.claim(cap)
    }
}

impl ReceivedResources for () {
    type Handle<K: ObjectKind> = core::convert::Infallible;

    fn claim<K: ObjectKind>(&mut self, _cap: Cap<K>) -> Option<Self::Handle<K>> {
        None
    }

    fn retain_decoded(&mut self, _slots: &HandleSlots) {}
}
