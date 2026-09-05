//! Владеющая (`OwnedHandle`) и заимствующая (`BorrowedHandle`) обёртки хэндла.

use core::{marker::PhantomData, mem::ManuallyDrop, num::NonZeroU64};

use ipc::IntoWireHandle;
use syscall::{Handle, RawHandle, Rights};

use crate::{
    error::{Error, Result, unit},
    svc,
};

const UNBADGED: u64 = 0;

/// Владеет хэндлом и закрывает его в `Drop`.
///
/// Не реализует `Copy` и`Clone`, в отличии от `BorrowedHandle`.
#[derive(Debug)]
pub struct OwnedHandle {
    handle: Handle,
}

/// Заимствование хэндла на время одного вызова, привязанное к времени жизни
/// владельца.
#[derive(Debug, Clone, Copy)]
pub struct BorrowedHandle<'a> {
    handle: Handle,
    owner: PhantomData<&'a OwnedHandle>,
}

impl OwnedHandle {
    /// Берёт хэндл во владение и закрывает его в `Drop`.
    pub fn from_handle(handle: Handle) -> Self {
        Self { handle }
    }

    /// Усыновляет сырой HandleId, помещённый ядром в таблицу процесса. `None`
    /// на нуле (невалидный handle).
    pub fn from_raw(raw: RawHandle) -> Option<Self> {
        Handle::new(raw).map(Self::from_handle)
    }

    /// Владеемый хэндл для укладки в аргумент syscall.
    pub fn as_raw(&self) -> Handle {
        self.handle
    }

    /// Заимствование, не переживающее владельца.
    pub fn borrow(&self) -> BorrowedHandle<'_> {
        BorrowedHandle {
            handle: self.handle,
            owner: PhantomData,
        }
    }

    /// Дублирует через заимствование.
    pub fn duplicate(&self, rights: Rights, badge: Option<NonZeroU64>) -> Result<OwnedHandle> {
        self.borrow().duplicate(rights, badge)
    }

    /// Закрывает хэндл, возвращая результат syscall. Извлекает хэндл через
    /// `into_raw` до `svc::handle_close`, и `Drop` его повторно не закрывает.
    pub fn close(self) -> Result<()> {
        unit(svc::handle_close(self.into_raw()))
    }

    /// Отдаёт владеемый хэндл и подавляет `Drop` - единственный примитив
    /// извлечения. Все потребляющие хэндл операции проходят через него.
    pub fn into_raw(self) -> Handle {
        ManuallyDrop::new(self).handle
    }
}

impl Drop for OwnedHandle {
    fn drop(&mut self) {
        let _ = svc::handle_close(self.handle);
    }
}

impl From<syscall_user::ReceivedHandle> for OwnedHandle {
    fn from(received: syscall_user::ReceivedHandle) -> Self {
        Self::from_handle(received.into_raw())
    }
}

impl IntoWireHandle for OwnedHandle {
    fn wire_id(&self) -> u32 {
        self.handle.raw()
    }

    fn relinquish(self) {
        // Ядро забрало хэндл, close подавляется; неуспех передачи закрывает
        // хэндл через Drop.
        let _ = self.into_raw();
    }
}

impl From<BorrowedHandle<'_>> for Handle {
    fn from(borrowed: BorrowedHandle<'_>) -> Self {
        borrowed.handle
    }
}

impl From<&OwnedHandle> for Handle {
    fn from(owned: &OwnedHandle) -> Self {
        owned.handle
    }
}

impl BorrowedHandle<'_> {
    /// Заимствованный `Handle`.
    pub fn as_raw(self) -> Handle {
        self.handle
    }

    /// Дублирует хэндл с правами `rights` (подмножество исходных) и значком `badge`.
    /// Закрытие дубликата не трогает оригинал. Требует право `DUPLICATE`.
    /// Возвращает независимый `OwnedHandle`.
    pub fn duplicate(self, rights: Rights, badge: Option<NonZeroU64>) -> Result<OwnedHandle> {
        svc::handle_duplicate(
            self.handle,
            rights.into(),
            // Отсутствие значка ABI задаёт нулём: производная capability наследует значок
            // источника.
            badge.map_or(UNBADGED, NonZeroU64::get),
        )
        .map(OwnedHandle::from_handle)
        .map_err(Error::Syscall)
    }
}
