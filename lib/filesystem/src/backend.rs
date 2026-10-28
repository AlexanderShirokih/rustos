//! Общая часть файловых систем: операции над деревом имён и над содержимым узлов.

use filesystem_abi::{DirCursor, FileSystemError, Listed, NodeKind, Path};

use crate::{
    node::{NodeId, Window},
    store::{ByteStore, StoreId},
};

/// Записи каталога, уложенные в буфер одним перечислением.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct DirBatch {
    /// Итог перечисления: число записей, позиция продолжения и признак исчерпания.
    pub listed: Listed,
    /// Длина уложенных записей от начала буфера.
    pub len: usize,
}

/// Вид узла бекенда и его размер.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct NodeMetadata {
    /// Вид узла.
    pub kind: NodeKind,
    /// Размер узла: у файла - длина содержимого в байтах, у каталога - число его записей.
    pub size: u64,
}

/// Файловая система точки монтирования. Пути отсчитываются от её корня, [`NodeId`] действителен
/// только внутри неё.
///
/// Вызывающий передаёт путь, проверенный [`Path::new`] и пересаженный в окно сессии, и
/// [`NodeId`], выданный этим же бекендом. Значение, которого система не содержит, любая операция
/// по узлу отвергает ответом `NotFound`. Таблица хранилищ приходит параметром `stores`, и байты
/// файлов реализация читает и пишет только через неё.
///
/// Узел существует, пока за ним закреплено имя либо остался хотя бы один держатель. Снятие
/// имени оставляет в системе узел с держателями, и последний [`release`](Self::release) его
/// освобождает.
pub trait FileSystemBackend {
    /// Допускает ли файловая система изменения. Реализация отвечает одним значением всё время
    /// своей работы. Режим дескриптора проверяется на открытии, и запись в неизменяемое дерево
    /// получает отказ уже там.
    fn is_mutable(&self) -> bool;

    /// Возвращает узел по пути. Корень дерева существует всегда. Если дерево не содержит такого
    /// имени, возвращает `NotFound`. Если компонент пути оказался файлом, возвращает
    /// `NotADirectory`.
    fn lookup(&self, path: Path<'_>) -> Result<NodeId, FileSystemError>;

    /// Создаёт узел вида `kind` под именем последнего компонента пути и возвращает его. Имя
    /// закрепляется за узлом той же операцией, и держателей у созданного узла ноль. Если имя
    /// занято, возвращает `AlreadyExists`. Если каталог над именем отсутствует, возвращает
    /// `NotFound`. Если файловая система неизменяема, возвращает `ReadOnly`.
    fn create(&mut self, path: Path<'_>, kind: NodeKind) -> Result<NodeId, FileSystemError>;

    /// Снимает имя с узла вида `expected`. Вид сравнивается там же, где снимается имя. Узел с
    /// держателями остаётся в системе, и открытые по нему [`NodeId`] действительны до последнего
    /// [`release`](Self::release). Узел без держателей реализация освобождает вместе с его
    /// хранилищем в `stores`.
    ///
    /// Если узел по пути отсутствует, возвращает `NotFound`. Если вид не совпал, возвращает
    /// `IsADirectory` при ожидании файла и `NotADirectory` при ожидании каталога. Если это
    /// каталог с записями, возвращает `NotEmpty`. Если файловая система неизменяема,
    /// возвращает `ReadOnly`.
    fn unlink(
        &mut self,
        stores: &mut dyn ByteStore,
        path: Path<'_>,
        expected: NodeKind,
    ) -> Result<(), FileSystemError>;

    /// Переносит узел под новое имя, замещая занятое. Замещённый узел теряет имя по правилам
    /// [`unlink`](Self::unlink). Совпадение `from` с `to` дерево оставляет как есть.
    ///
    /// Если узел `from` отсутствует, возвращает `NotFound`. Если имя `to` занято каталогом,
    /// возвращает `IsADirectory`. Если каталог переносится внутрь собственного поддерева,
    /// возвращает `InvalidPath`. Если файловая система неизменяема, возвращает `ReadOnly`.
    fn rename(
        &mut self,
        stores: &mut dyn ByteStore,
        from: Path<'_>,
        to: Path<'_>,
    ) -> Result<(), FileSystemError>;

    /// Ставит на узел держателя. Каждому вызову отвечает свой [`release`](Self::release). Если
    /// узел в системе отсутствует, возвращает `NotFound`. Если счётчик держателей исчерпан,
    /// возвращает `TooManyOpenFiles`.
    fn retain(&mut self, node: NodeId) -> Result<(), FileSystemError>;

    /// Снимает с узла держателя. Узел без имени и без держателей реализация освобождает вместе с
    /// его хранилищем в `stores`. Вызов по узлу вне системы проходит без последствий.
    fn release(&mut self, stores: &mut dyn ByteStore, node: NodeId);

    /// Возвращает вид узла и его размер: длину файла в байтах либо число записей каталога. Если
    /// узел в системе отсутствует, возвращает `NotFound`.
    fn metadata(&self, node: NodeId) -> Result<NodeMetadata, FileSystemError>;

    /// Возвращает регион содержимого файла. Действующую длину задаёт metadata.
    /// пустой файл без региона даёт NotSupported, каталог - IsADirectory.
    fn read_window(&self, node: NodeId) -> Result<Window, FileSystemError>;

    /// Резервирует диапазон записи, не публикуя новую длину файла. Рост сохраняет прежние
    /// байты, промежуток до offset заполнен нулями. Вызывающий исключает конкурирующие
    /// изменения узла до commit_write либо отказа от окна.
    fn prepare_write(
        &mut self,
        stores: &mut dyn ByteStore,
        node: NodeId,
        offset: u64,
        len: usize,
    ) -> Result<Window, FileSystemError>;

    /// Публикует конец перенесённого префикса. Запись внутри длины её не уменьшает.
    fn commit_write(&mut self, node: NodeId, end: u64) -> Result<(), FileSystemError>;

    /// Задаёт длину файла. Байты за `size` отбрасываются, недостающие дописываются нулями. Если
    /// `size` больше предела, установленного файловой системой, возвращает `NoSpace`. Если узел
    /// оказался каталогом, возвращает `IsADirectory`.
    fn set_size(
        &mut self,
        stores: &mut dyn ByteStore,
        node: NodeId,
        size: u64,
    ) -> Result<(), FileSystemError>;

    /// Укладывает записи каталога подряд в начало `out`, начиная с позиции `from`, пока они
    /// помещаются, и возвращает итог перечисления и длину уложенных записей. `from` -
    /// [`DirCursor::START`] либо позиция, выданная прежним перечислением этого же узла, а
    /// трактовка значения принадлежит бекенду. Нулевое число записей значит, что буфер остался
    /// нетронутым. Такой итог без признака исчерпания значит, что очередная запись длиннее всего
    /// буфера, и позиция продолжения указывает на неё. Если узел оказался файлом, возвращает
    /// `NotADirectory`.
    fn list(
        &self,
        node: NodeId,
        from: DirCursor,
        out: &mut [u8],
    ) -> Result<DirBatch, FileSystemError>;

    /// Закрепляет ёмкость узла за его хранилищем после выдачи клиенту capability этого
    /// хранилища. Закреплённый узел за пределы ёмкости не растёт, и запись за неё возвращает
    /// `NoSpace`. Если узел хранилища не занимает, возвращает `NotSupported`.
    fn pin(&mut self, node: NodeId) -> Result<(), FileSystemError>;

    /// Создаёт файл под `path` поверх хранилища `store`, не копируя его байты. Владение
    /// хранилищем переходит файловой системе, и она освобождает его вместе с узлом. `size`
    /// задаёт начальную длину файла, `capacity` - предел, за который файл не растёт.
    ///
    /// Если `size` больше `capacity`, возвращает `NoSpace`. Если имя занято, возвращает
    /// `AlreadyExists`. Реализация без такой формы файла возвращает `NotSupported`.
    fn create_backed(
        &mut self,
        _path: Path<'_>,
        _store: StoreId,
        _size: u64,
        _capacity: u64,
    ) -> Result<NodeId, FileSystemError> {
        Err(FileSystemError::NotSupported)
    }

    /// Копирует доступную часть файла со смещения `offset` в `out`. Каталог даёт
    /// `IsADirectory`, чтение за концом файла возвращает ноль.
    fn read_at(
        &self,
        stores: &dyn ByteStore,
        node: NodeId,
        offset: u64,
        out: &mut [u8],
    ) -> Result<usize, FileSystemError> {
        let metadata = self.metadata(node)?;
        if metadata.kind.is_directory() {
            return Err(FileSystemError::IsADirectory);
        }

        let Ok(offset) = usize::try_from(offset) else {
            return Ok(0);
        };
        let size = usize::try_from(metadata.size).map_err(|_| FileSystemError::NoSpace)?;
        let count = size.saturating_sub(offset).min(out.len());
        if count == 0 {
            return Ok(0);
        }

        let window = self.read_window(node)?;
        stores.read(
            window.store,
            store_offset(window, offset)?,
            out.get_mut(..count)
                .ok_or(FileSystemError::BufferTooSmall)?,
        )?;
        Ok(count)
    }

    /// Копирует `data` в подготовленное окно и публикует конец записи после переноса байт.
    /// Пустой `data` длину узла не меняет. Вызывающий исключает конкурирующие изменения узла
    /// на время операции.
    fn write_at(
        &mut self,
        stores: &mut dyn ByteStore,
        node: NodeId,
        offset: u64,
        data: &[u8],
    ) -> Result<usize, FileSystemError> {
        if data.is_empty() {
            if !self.is_mutable() {
                return Err(FileSystemError::ReadOnly);
            }

            if self.metadata(node)?.kind.is_directory() {
                return Err(FileSystemError::IsADirectory);
            }

            return Ok(0);
        }

        let window = self.prepare_write(stores, node, offset, data.len())?;
        let len = u64::try_from(data.len()).map_err(|_| FileSystemError::NoSpace)?;
        let end = offset.checked_add(len).ok_or(FileSystemError::NoSpace)?;
        let offset = usize::try_from(offset).map_err(|_| FileSystemError::NoSpace)?;

        stores.write(window.store, store_offset(window, offset)?, data)?;
        self.commit_write(node, end)?;
        Ok(data.len())
    }

    /// Копирует `data` в конец файла и возвращает смещение записи. Вызывающий исключает
    /// конкурирующие изменения узла на время операции.
    fn append(
        &mut self,
        stores: &mut dyn ByteStore,
        node: NodeId,
        data: &[u8],
    ) -> Result<u64, FileSystemError> {
        let offset = self.metadata(node)?.size;
        self.write_at(stores, node, offset, data)?;
        Ok(offset)
    }
}

pub(crate) const fn kind_mismatch(expected: NodeKind) -> FileSystemError {
    match expected {
        NodeKind::File => FileSystemError::IsADirectory,
        NodeKind::Directory => FileSystemError::NotADirectory,
    }
}

fn store_offset(window: Window, offset: usize) -> Result<usize, FileSystemError> {
    let base = usize::try_from(window.offset).map_err(|_| FileSystemError::NoSpace)?;
    base.checked_add(offset).ok_or(FileSystemError::NoSpace)
}
