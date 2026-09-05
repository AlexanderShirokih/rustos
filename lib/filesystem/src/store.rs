//! Непрозрачное для модели хранилище байт файлов.

use alloc::vec::Vec;

use filesystem_abi::FileSystemError;

/// Ссылка на хранилище байт, действительная внутри одного файлового сервера.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct StoreId(u32);

impl StoreId {
    /// Слот региона точки монтирования. Занят, только если сервер запущен с таким регионом.
    pub const MOUNT: Self = Self(0);

    /// Строит идентификатор из номера слота серверной таблицы.
    pub const fn from_raw(raw: u32) -> Self {
        Self(raw)
    }

    /// Возвращает номер слота серверной таблицы.
    pub const fn raw(self) -> u32 {
        self.0
    }
}

/// Хранилище байт, чьё владение и способ доступа определяет файловый сервер.
pub trait ByteStore {
    /// Выделяет хранилище ровно на `size` байт.
    fn allocate(&mut self, size: usize) -> Result<StoreId, FileSystemError>;

    /// Копирует байты хранилища со смещения `offset` в `out`.
    fn read(&self, store: StoreId, offset: usize, out: &mut [u8]) -> Result<(), FileSystemError>;

    /// Копирует `data` в хранилище со смещения `offset`.
    fn write(&mut self, store: StoreId, offset: usize, data: &[u8]) -> Result<(), FileSystemError>;

    /// Заполняет нулями диапазон хранилища.
    fn zero(&mut self, store: StoreId, offset: usize, len: usize) -> Result<(), FileSystemError>;

    /// Освобождает хранилище. Неизвестный идентификатор игнорируется.
    fn release(&mut self, store: StoreId);
}

/// Кладёт значение в первый свободный слот таблицы хранилищ и возвращает его идентификатор.
pub fn insert_store<T>(slots: &mut Vec<Option<T>>, value: T) -> Result<StoreId, FileSystemError> {
    if let Some(index) = slots.iter().position(Option::is_none) {
        let id = StoreId::from_raw(u32::try_from(index).map_err(|_| FileSystemError::NoSpace)?);
        let slot = slots.get_mut(index).ok_or(FileSystemError::NoSpace)?;
        *slot = Some(value);
        return Ok(id);
    }
    slots.try_reserve(1).map_err(|_| FileSystemError::NoSpace)?;
    let id = StoreId::from_raw(u32::try_from(slots.len()).map_err(|_| FileSystemError::NoSpace)?);
    slots.push(Some(value));
    Ok(id)
}

/// Host-хранилище для тестов модели файловой системы.
#[derive(Default)]
pub struct HeapByteStore {
    stores: Vec<Option<Vec<u8>>>,
}

impl HeapByteStore {
    /// Пустая таблица хранилищ.
    pub const fn new() -> Self {
        Self { stores: Vec::new() }
    }

    fn bytes(&self, store: StoreId) -> Result<&[u8], FileSystemError> {
        self.stores
            .get(store.raw() as usize)
            .and_then(Option::as_deref)
            .ok_or(FileSystemError::NotFound)
    }

    fn bytes_mut(&mut self, store: StoreId) -> Result<&mut [u8], FileSystemError> {
        self.stores
            .get_mut(store.raw() as usize)
            .and_then(Option::as_deref_mut)
            .ok_or(FileSystemError::NotFound)
    }
}

impl ByteStore for HeapByteStore {
    fn allocate(&mut self, size: usize) -> Result<StoreId, FileSystemError> {
        let mut bytes = Vec::new();
        bytes
            .try_reserve_exact(size)
            .map_err(|_| FileSystemError::NoSpace)?;
        bytes.resize(size, 0);
        insert_store(&mut self.stores, bytes)
    }

    fn read(&self, store: StoreId, offset: usize, out: &mut [u8]) -> Result<(), FileSystemError> {
        let end = offset
            .checked_add(out.len())
            .ok_or(FileSystemError::NoSpace)?;
        let source = self
            .bytes(store)?
            .get(offset..end)
            .ok_or(FileSystemError::NoSpace)?;
        out.copy_from_slice(source);
        Ok(())
    }

    fn write(&mut self, store: StoreId, offset: usize, data: &[u8]) -> Result<(), FileSystemError> {
        let end = offset
            .checked_add(data.len())
            .ok_or(FileSystemError::NoSpace)?;
        let target = self
            .bytes_mut(store)?
            .get_mut(offset..end)
            .ok_or(FileSystemError::NoSpace)?;
        target.copy_from_slice(data);
        Ok(())
    }

    fn zero(&mut self, store: StoreId, offset: usize, len: usize) -> Result<(), FileSystemError> {
        let end = offset.checked_add(len).ok_or(FileSystemError::NoSpace)?;
        self.bytes_mut(store)?
            .get_mut(offset..end)
            .ok_or(FileSystemError::NoSpace)?
            .fill(0);
        Ok(())
    }

    fn release(&mut self, store: StoreId) {
        if let Some(slot) = self.stores.get_mut(store.raw() as usize) {
            *slot = None;
        }
    }
}

#[cfg(test)]
pub(crate) mod host {
    use core::ops::{Deref, DerefMut};

    use filesystem_abi::{FileSystemError, NodeKind, Path};

    use super::HeapByteStore;
    use crate::{
        backend::FileSystemBackend,
        node::{NodeId, Window},
    };

    pub(crate) struct WithStore<B> {
        pub(crate) backend: B,
        pub(crate) stores: HeapByteStore,
    }

    impl<B: FileSystemBackend> WithStore<B> {
        pub(crate) fn read_at(
            &self,
            node: NodeId,
            offset: u64,
            out: &mut [u8],
        ) -> Result<usize, FileSystemError> {
            self.backend.read_at(&self.stores, node, offset, out)
        }

        pub(crate) fn write_at(
            &mut self,
            node: NodeId,
            offset: u64,
            data: &[u8],
        ) -> Result<usize, FileSystemError> {
            self.backend.write_at(&mut self.stores, node, offset, data)
        }

        pub(crate) fn append(&mut self, node: NodeId, data: &[u8]) -> Result<u64, FileSystemError> {
            self.backend.append(&mut self.stores, node, data)
        }

        pub(crate) fn set_size(&mut self, node: NodeId, size: u64) -> Result<(), FileSystemError> {
            self.backend.set_size(&mut self.stores, node, size)
        }

        pub(crate) fn backing(&mut self, node: NodeId) -> Result<Window, FileSystemError> {
            self.backend.backing(&mut self.stores, node)
        }

        pub(crate) fn unlink(
            &mut self,
            path: Path<'_>,
            expected: NodeKind,
        ) -> Result<(), FileSystemError> {
            self.backend.unlink(&mut self.stores, path, expected)
        }

        pub(crate) fn rename(
            &mut self,
            from: Path<'_>,
            to: Path<'_>,
        ) -> Result<(), FileSystemError> {
            self.backend.rename(&mut self.stores, from, to)
        }

        pub(crate) fn release(&mut self, node: NodeId) {
            self.backend.release(&mut self.stores, node);
        }
    }

    impl<B> Deref for WithStore<B> {
        type Target = B;

        fn deref(&self) -> &Self::Target {
            &self.backend
        }
    }

    impl<B> DerefMut for WithStore<B> {
        fn deref_mut(&mut self) -> &mut Self::Target {
            &mut self.backend
        }
    }
}
