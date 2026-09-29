//! Владелец записи таблицы handle, выданной ядром.

use core::{fmt, mem::ManuallyDrop, num::NonZeroU32};

use ipc::{
    IntoWireValue,
    wire::{Cap, IpcError, MessageBuf},
};
use syscall::{Handle, ObjectKind, SyscallError, kind};

use crate::{Delivery, svc};

/// Единственный владелец записи таблицы handle вида `K`.
///
/// Владельца отдаёт только выдача ядром: выдающая операция `svc` либо доставка кадра.
/// Разрушение владельца закрывает запись. Изымающие запись операции `svc` принимают владельца
/// значением, а номер для операций, которые запись не изымают, отдаёт [`IssuedHandle::as_raw`].
pub struct IssuedHandle<K: ObjectKind> {
    handle: Handle<K>,
}

impl<K: ObjectKind> IssuedHandle<K> {
    pub(crate) const fn new(handle: Handle<K>) -> Self {
        Self { handle }
    }

    pub(crate) fn from_syscall_return(ret: i64) -> Result<Self, SyscallError> {
        Handle::from_syscall_return(ret).map(Self::new)
    }

    /// Номер записи для операции, которая запись не изымает.
    #[must_use]
    pub const fn as_raw(&self) -> Handle<K> {
        self.handle
    }

    /// Оставляет запись процессу до его завершения и отдаёт её номер. Запись закрывает ядро
    /// при завершении процесса.
    #[must_use]
    pub fn leak(self) -> Handle<K> {
        ManuallyDrop::new(self).handle
    }

    /// Владелец той же записи без статического вида.
    #[must_use]
    pub fn erase(self) -> IssuedHandle<kind::Any> {
        IssuedHandle::new(self.leak().erase())
    }
}

impl IssuedHandle<kind::Any> {
    /// Владелец записи по её номеру. Номер приходит вне типов библиотеки: из регистра `x0`
    /// точки входа процесса, из IPC-буфера потока либо от владельца в другой копии библиотеки,
    /// который отказался от записи через [`IssuedHandle::leak`].
    ///
    /// # Safety
    ///
    /// `raw` называет открытую запись таблицы процесса, и у неё нет другого владельца.
    #[must_use]
    pub unsafe fn adopt(raw: NonZeroU32) -> Self {
        Self::new(Handle::from_abi(raw))
    }

    /// Владелец той же записи с заявленным видом `K`. Операция по записи другого вида
    /// получает от ядра `WrongType`.
    #[must_use]
    pub fn assume_kind<K: ObjectKind>(self) -> IssuedHandle<K> {
        IssuedHandle::new(Handle::from_abi(self.leak().as_nonzero()))
    }
}

impl<K: ObjectKind> IntoWireValue<Delivery> for IssuedHandle<K> {
    type Read = Cap<K>;

    fn write_into(self, frame: &mut MessageBuf<Delivery>, field_id: u8) -> Result<(), IpcError> {
        frame.write_handle(field_id, self)
    }
}

impl<K: ObjectKind> fmt::Debug for IssuedHandle<K> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_tuple("IssuedHandle").field(&self.handle).finish()
    }
}

impl<K: ObjectKind> Drop for IssuedHandle<K> {
    fn drop(&mut self) {
        let _ = svc::close_entry(self.handle);
    }
}
