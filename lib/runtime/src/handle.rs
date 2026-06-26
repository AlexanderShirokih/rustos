//! Владеющая (`OwnedHandle`) и заимствующая (`BorrowedHandle`) обёртки хэндла.

use core::{marker::PhantomData, mem::ManuallyDrop};

use syscall::{Handle, RawHandle, Rights};

use crate::{
    error::{Error, Result, unit},
    svc,
};

/// Владеет хэндлом и закрывает его в `Drop`.
///
/// Не `Copy` и не `Clone`: владелец один.
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
    /// Берёт во владение свежий хэндл из syscall'а.
    ///
    /// # Safety
    /// Вызывающий гарантирует уникальное владение `handle`; двойное оборачивание
    /// ведёт к преждевременному close (другой владелец получит `BadHandle`).
    pub unsafe fn from_handle(handle: Handle) -> Self {
        Self { handle }
    }

    /// Усыновляет сырой HandleId, помещённый ядром в таблицу процесса; `None`
    /// на нуле (невалидный handle).
    ///
    /// # Safety
    /// Вызывающий гарантирует уникальное владение `raw`; двойное оборачивание
    /// ведёт к преждевременному close (другой владелец получит `BadHandle`).
    pub unsafe fn from_raw(raw: RawHandle) -> Option<Self> {
        // SAFETY: усыновляем тот же хэндл, уникальность гарантирует вызывающий.
        Handle::new(raw).map(|handle| unsafe { Self::from_handle(handle) })
    }

    /// Владеемый хэндл для укладки в аргумент syscall'а.
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

    /// Дублирует хэндл с правами `rights` (подмножество исходных) и значком
    /// `badge` (set-once). Возвращает независимый хэндл; закрытие дубликата не
    /// трогает оригинал. Требует право `DUPLICATE`.
    pub fn duplicate(&self, rights: Rights, badge: u64) -> Result<OwnedHandle> {
        svc::handle_duplicate(self.handle, rights.into(), badge)
            // SAFETY: handle_duplicate вернул свежий хэндл, мы единственный владелец.
            .map(|handle| unsafe { Self::from_handle(handle) })
            .map_err(Error::Syscall)
    }

    /// Закрывает хэндл, возвращая результат syscall'а. Извлекает хэндл через
    /// `into_raw` до `svc::handle_close`, иначе `Drop` закрыл бы его повторно.
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

impl BorrowedHandle<'_> {
    /// Заимствованный `Handle`.
    pub fn as_raw(self) -> Handle {
        self.handle
    }
}
