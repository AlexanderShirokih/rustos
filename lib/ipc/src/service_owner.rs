use alloc::rc::Rc;
use core::ops::Deref;

/// Владелец сервиса, клон которого забирает future асинхронной операции.
pub trait ServiceOwner: Clone + 'static {
    /// Сервис, которому `serve` отдаёт операции.
    type Service;

    /// Сервис владельца.
    fn service(&self) -> &Self::Service;
}

impl<T: 'static> ServiceOwner for Rc<T> {
    type Service = T;

    fn service(&self) -> &T {
        self
    }
}

#[doc(hidden)]
pub struct __Owned<O>(pub O);

impl<O: ServiceOwner> Deref for __Owned<O> {
    type Target = O::Service;

    fn deref(&self) -> &O::Service {
        self.0.service()
    }
}
