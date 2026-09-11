//! Владеющая (`OwnedHandle`) и заимствующая (`BorrowedHandle`) обёртки хэндла.

use core::{fmt, marker::PhantomData, mem::ManuallyDrop, num::NonZeroU64};

use ipc::{
    IntoWireValue,
    wire::{Cap, IpcError, MessageBuf},
};
use syscall::{Handle, ObjectKind, Rights, kind};

use crate::{
    error::{Error, Result, unit},
    svc,
};

const UNBADGED: u64 = 0;

/// Владеет хэндлом объекта вида `K` и закрывает его в `Drop`.
///
/// Не реализует `Copy` и`Clone`, в отличии от `BorrowedHandle`. Предметные имена
/// специализаций объявлены в [`crate`], а операции объекта - их методами.
///
/// Предметное имя называет ту же специализацию:
///
/// ```no_run
/// use core::num::NonZeroU32;
/// use kernel_objects::{OwnedHandle, Port, kind};
/// use syscall::Handle;
///
/// let raw = Handle::<kind::Port>::from_abi(NonZeroU32::new(7).unwrap());
/// let port: Port = OwnedHandle::from_handle(raw);
/// let _: OwnedHandle<kind::Port> = port;
/// ```
///
/// Владелец чужого вида под этим именем не проходит:
///
/// ```compile_fail
/// use core::num::NonZeroU32;
/// use kernel_objects::{OwnedHandle, Port, kind};
/// use syscall::Handle;
///
/// let raw = Handle::<kind::Signal>::from_abi(NonZeroU32::new(7).unwrap());
/// let _: Port = OwnedHandle::from_handle(raw);
/// ```
pub struct OwnedHandle<K: ObjectKind> {
    handle: Handle<K>,
}

/// Заимствование хэндла на время одного вызова, привязанное к времени жизни
/// владельца. Вид объекта сохраняется.
pub struct BorrowedHandle<'a, K: ObjectKind> {
    handle: Handle<K>,
    owner: PhantomData<&'a OwnedHandle<K>>,
}

impl<K: ObjectKind> OwnedHandle<K> {
    /// Берёт хэндл во владение и закрывает его в `Drop`. Вызывается там, где запись
    /// действительно передаётся новому владельцу.
    pub fn from_handle(handle: Handle<K>) -> Self {
        Self { handle }
    }

    /// Владеемый хэндл для укладки в аргумент syscall.
    pub fn as_raw(&self) -> Handle<K> {
        self.handle
    }

    /// Заимствование, не переживающее владельца.
    pub fn borrow(&self) -> BorrowedHandle<'_, K> {
        BorrowedHandle {
            handle: self.handle,
            owner: PhantomData,
        }
    }

    /// Дублирует хэндл с правами `rights` (подмножество исходных) и значком `badge`.
    /// Требует право `DUPLICATE`. Возвращает независимый `OwnedHandle` того же вида,
    /// и закрытие дубликата не трогает оригинал.
    ///
    /// ```no_run
    /// use core::num::NonZeroU32;
    /// use kernel_objects::{OwnedHandle, Port, kind};
    /// use syscall::{Handle, Rights};
    ///
    /// let port = Port::from_handle(Handle::from_abi(NonZeroU32::new(5).unwrap()));
    /// let copy: OwnedHandle<kind::Port> = port.duplicate(Rights::WRITE, None).unwrap();
    /// let _: Port = copy;
    /// ```
    pub fn duplicate(&self, rights: Rights, badge: Option<NonZeroU64>) -> Result<OwnedHandle<K>> {
        self.borrow().duplicate(rights, badge)
    }

    /// Закрывает хэндл, возвращая результат syscall. Извлекает хэндл через
    /// `into_raw` до `svc::handle_close`, и `Drop` его повторно не закрывает.
    pub fn close(self) -> Result<()> {
        unit(svc::handle_close(self.into_raw()))
    }

    /// Отдаёт владеемый хэндл и подавляет `Drop` - единственный примитив
    /// извлечения. Все потребляющие хэндл операции проходят через него.
    pub fn into_raw(self) -> Handle<K> {
        ManuallyDrop::new(self).handle
    }

    /// Передаёт владение в гетерогенное хранилище, снимая статическую метку вида.
    /// Идентификатор и однократность владения сохраняются.
    ///
    /// ```no_run
    /// use core::num::NonZeroU32;
    /// use kernel_objects::{OwnedHandle, Port, kind};
    /// use syscall::Handle;
    ///
    /// let port = Port::from_handle(Handle::from_abi(NonZeroU32::new(9).unwrap()));
    /// let any: OwnedHandle<kind::Any> = port.erase();
    /// assert_eq!(any.as_raw().raw(), 9);
    /// ```
    ///
    /// Обратного восстановления вида обычный API не даёт:
    ///
    /// ```compile_fail
    /// use core::num::NonZeroU32;
    /// use kernel_objects::{OwnedHandle, Port, kind};
    /// use syscall::Handle;
    ///
    /// let port = Port::from_handle(Handle::from_abi(NonZeroU32::new(9).unwrap()));
    /// let _: OwnedHandle<kind::Port> = port.erase();
    /// ```
    pub fn erase(self) -> OwnedHandle<kind::Any> {
        OwnedHandle::from_handle(self.into_raw().erase())
    }
}

impl<K: ObjectKind> fmt::Debug for OwnedHandle<K> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_tuple("OwnedHandle").field(&self.handle).finish()
    }
}

impl<K: ObjectKind> Drop for OwnedHandle<K> {
    fn drop(&mut self) {
        let _ = svc::handle_close(self.handle);
    }
}

impl<K: ObjectKind> From<syscall_user::ReceivedHandle<K>> for OwnedHandle<K> {
    fn from(received: syscall_user::ReceivedHandle<K>) -> Self {
        Self::from_handle(received.into_raw())
    }
}

impl<K: ObjectKind> IntoWireValue for OwnedHandle<K> {
    type Read = Cap<K>;

    fn write_as_field(
        &self,
        buf: &mut MessageBuf,
        field_id: u8,
    ) -> core::result::Result<(), IpcError> {
        ipc::IntoWireValue::write_as_field(&Cap::from_handle(self.handle), buf, field_id)
    }

    fn relinquish(self) {
        // Ядро забрало хэндл, close подавляется. Неуспех передачи закрывает
        // хэндл через Drop.
        let _ = self.into_raw();
    }
}

impl<K: ObjectKind> From<BorrowedHandle<'_, K>> for Handle<K> {
    fn from(borrowed: BorrowedHandle<'_, K>) -> Self {
        borrowed.handle
    }
}

impl<K: ObjectKind> From<&OwnedHandle<K>> for Handle<K> {
    fn from(owned: &OwnedHandle<K>) -> Self {
        owned.handle
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
    ///
    /// ```compile_fail
    /// use core::num::NonZeroU32;
    /// use kernel_objects::{Port, kind};
    /// use syscall::Handle;
    ///
    /// let erased;
    /// {
    ///     let port = Port::from_handle(Handle::from_abi(NonZeroU32::MIN));
    ///     erased = port.borrow().erase();
    /// }
    /// let _: Handle<kind::Any> = erased.into();
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
        .map(OwnedHandle::from_handle)
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
