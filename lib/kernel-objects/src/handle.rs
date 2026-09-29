//! Владеющая (`OwnedHandle`) и заимствующая (`BorrowedHandle`) обёртки хэндла.

use core::{fmt, marker::PhantomData, num::NonZeroU64};

use ipc::{
    IntoWireValue,
    wire::{Cap, IpcError, MessageBuf},
};
use syscall::{Handle, ObjectKind, Rights, kind};
use syscall_user::{Delivery, IssuedHandle};

use crate::{
    error::{Error, Result, unit},
    svc,
};

const UNBADGED: u64 = 0;

/// Владеет хэндлом объекта вида `K`. Запись закрывает вложенный владелец
/// [`IssuedHandle`](syscall_user::IssuedHandle) при разрушении.
///
/// Не реализует `Copy` и `Clone`, в отличие от `BorrowedHandle`. Владельца строит только
/// выдача ядром. Предметные имена специализаций объявлены в [`crate`], а операции объекта -
/// их методами.
///
/// Предметное имя называет ту же специализацию:
///
/// ```no_run
/// use kernel_objects::{OwnedHandle, Port, kind};
///
/// let port: Port = Port::create().unwrap();
/// let _: OwnedHandle<kind::Port> = port;
/// ```
pub struct OwnedHandle<K: ObjectKind> {
    handle: IssuedHandle<K>,
}

/// Заимствование хэндла на время одного вызова, привязанное к времени жизни
/// владельца. Вид объекта сохраняется.
pub struct BorrowedHandle<'a, K: ObjectKind> {
    handle: Handle<K>,
    owner: PhantomData<&'a OwnedHandle<K>>,
}

impl<K: ObjectKind> OwnedHandle<K> {
    /// Владеемый хэндл для укладки в аргумент syscall.
    pub fn as_raw(&self) -> Handle<K> {
        self.handle.as_raw()
    }

    /// Заимствование, не переживающее владельца.
    pub fn borrow(&self) -> BorrowedHandle<'_, K> {
        BorrowedHandle {
            handle: self.as_raw(),
            owner: PhantomData,
        }
    }

    /// Дублирует хэндл с правами `rights` (подмножество исходных) и значком `badge`.
    /// Требует право `DUPLICATE`. Возвращает независимый `OwnedHandle` того же вида,
    /// и закрытие дубликата не трогает оригинал.
    ///
    /// ```no_run
    /// use kernel_objects::Port;
    /// use syscall::Rights;
    ///
    /// let port = Port::create().unwrap();
    /// let _: Port = port.duplicate(Rights::WRITE, None).unwrap();
    /// ```
    pub fn duplicate(&self, rights: Rights, badge: Option<NonZeroU64>) -> Result<OwnedHandle<K>> {
        self.borrow().duplicate(rights, badge)
    }

    /// Закрывает хэндл и возвращает результат syscall.
    pub fn close(self) -> Result<()> {
        unit(svc::handle_close(self.handle))
    }

    /// Оставляет запись процессу до его завершения и отдаёт её номер. Запись закрывает ядро
    /// при завершении процесса.
    pub fn into_raw(self) -> Handle<K> {
        self.handle.leak()
    }

    /// Вложенный владелец записи для изымающей операции `svc` или `runtime`.
    #[must_use]
    pub fn into_issued(self) -> IssuedHandle<K> {
        self.handle
    }

    /// Передаёт владение в гетерогенное хранилище, снимая статическую метку вида.
    /// Идентификатор и однократность владения сохраняются.
    ///
    /// ```no_run
    /// use kernel_objects::{OwnedHandle, Port, kind};
    ///
    /// let any: OwnedHandle<kind::Any> = Port::create().unwrap().erase();
    /// ```
    pub fn erase(self) -> OwnedHandle<kind::Any> {
        OwnedHandle {
            handle: self.handle.erase(),
        }
    }
}

impl OwnedHandle<kind::Any> {
    /// Владелец той же записи с заявленным видом `K`. Операция по записи другого вида
    /// получает от ядра `WrongType`.
    ///
    /// ```no_run
    /// use kernel_objects::{OwnedHandle, Port, kind};
    ///
    /// let any: OwnedHandle<kind::Any> = Port::create().unwrap().erase();
    /// let _: Port = any.assume_kind();
    /// ```
    pub fn assume_kind<K: ObjectKind>(self) -> OwnedHandle<K> {
        OwnedHandle {
            handle: self.handle.assume_kind(),
        }
    }
}

impl<K: ObjectKind> fmt::Debug for OwnedHandle<K> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_tuple("OwnedHandle").field(&self.as_raw()).finish()
    }
}

impl<K: ObjectKind> From<IssuedHandle<K>> for OwnedHandle<K> {
    fn from(handle: IssuedHandle<K>) -> Self {
        Self { handle }
    }
}

impl<K: ObjectKind> IntoWireValue<Delivery> for OwnedHandle<K> {
    type Read = Cap<K>;

    fn write_into(
        self,
        frame: &mut MessageBuf<Delivery>,
        field_id: u8,
    ) -> core::result::Result<(), IpcError> {
        self.handle.write_into(frame, field_id)
    }
}

impl<K: ObjectKind> From<BorrowedHandle<'_, K>> for Handle<K> {
    fn from(borrowed: BorrowedHandle<'_, K>) -> Self {
        borrowed.handle
    }
}

impl<K: ObjectKind> From<&OwnedHandle<K>> for Handle<K> {
    fn from(owned: &OwnedHandle<K>) -> Self {
        owned.as_raw()
    }
}

impl<'a, K: ObjectKind> BorrowedHandle<'a, K> {
    /// Заимствованный `Handle` вместе с его видом.
    pub fn as_raw(self) -> Handle<K> {
        self.handle
    }

    /// Снимает статическую метку вида, сохраняя идентификатор и время жизни заимствования.
    ///
    /// ```no_run
    /// use kernel_objects::{BorrowedHandle, OwnedHandle, Port, kind};
    /// use syscall::Rights;
    ///
    /// fn duplicate_erased(port: &Port) -> kernel_objects::Result<OwnedHandle<kind::Any>> {
    ///     let erased: BorrowedHandle<'_, kind::Any> = port.borrow().erase();
    ///     erased.duplicate(Rights::WRITE, None)
    /// }
    /// ```
    pub fn erase(self) -> BorrowedHandle<'a, kind::Any> {
        BorrowedHandle {
            handle: self.handle.erase(),
            owner: PhantomData,
        }
    }

    /// Дублирует хэндл с правами `rights` (подмножество исходных) и значком `badge`.
    /// Закрытие дубликата не трогает оригинал. Требует право `DUPLICATE`.
    /// Возвращает независимый `OwnedHandle` того же вида.
    pub fn duplicate(self, rights: Rights, badge: Option<NonZeroU64>) -> Result<OwnedHandle<K>> {
        svc::handle_duplicate(
            self.handle,
            rights.into(),
            // ABI задаёт отсутствие значка нулём. Производная capability наследует значок
            // источника.
            badge.map_or(UNBADGED, NonZeroU64::get),
        )
        .map(OwnedHandle::from)
        .map_err(Error::Syscall)
    }
}

impl<K: ObjectKind> Clone for BorrowedHandle<'_, K> {
    fn clone(&self) -> Self {
        *self
    }
}

impl<K: ObjectKind> Copy for BorrowedHandle<'_, K> {}

impl<K: ObjectKind> fmt::Debug for BorrowedHandle<'_, K> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_tuple("BorrowedHandle").field(&self.handle).finish()
    }
}
