//! Общая часть файловых систем: операции над деревом имён и над содержимым узлов.

use filesystem_abi::{DirCursor, FileSystemError, Listed, NodeInfo, NodeKind, Path};

use crate::{
    node::{NodeId, Window},
    store::{ByteStore, StoreId},
};

/// Файловая система точки монтирования. Пути отсчитываются от её корня, [`NodeId`] действителен
/// только внутри неё.
pub trait FileSystemBackend {
    /// Допускает ли файловая система изменения. Режим дескриптора проверяется на открытии:
    /// запись в неизменяемое дерево отказывает уже там.
    fn is_mutable(&self) -> bool;

    /// Возвращает узел по пути. Если имени в дереве нет, возвращает `NotFound`. Если компонент
    /// пути оказался файлом, возвращает `NotADirectory`.
    fn lookup(&self, path: Path<'_>) -> Result<NodeId, FileSystemError>;

    /// Заводит узел вида `kind` и возвращает его. Если имя занято, возвращает `AlreadyExists`.
    /// Если файловая система неизменяема, возвращает `ReadOnly`.
    fn create(&mut self, path: Path<'_>, kind: NodeKind) -> Result<NodeId, FileSystemError>;

    /// Снимает имя с узла. Если узла по пути нет, возвращает `NotFound`. Если это каталог с
    /// записями, возвращает `NotEmpty`. Если файловая система неизменяема, возвращает `ReadOnly`.
    fn unlink(&mut self, path: Path<'_>) -> Result<(), FileSystemError>;

    /// Переносит узел под новое имя, замещая занятое. Если узла `from` нет, возвращает `NotFound`.
    /// Если имя `to` занято каталогом, возвращает `IsADirectory`. Если файловая система
    /// неизменяема, возвращает `ReadOnly`.
    fn rename(&mut self, from: Path<'_>, to: Path<'_>) -> Result<(), FileSystemError>;

    /// Возвращает вид узла и его размер: длину файла в байтах либо число записей каталога. Если
    /// узла в системе нет, возвращает `NotFound`.
    fn metadata(&self, node: NodeId) -> Result<NodeInfo, FileSystemError>;

    /// Копирует в `out` байты файла, начиная со смещения `offset`, и возвращает их число.
    /// Смещение от длины файла и дальше даёт ноль байт. Если узел оказался каталогом, возвращает
    /// `IsADirectory`.
    fn read_at(
        &self,
        stores: &dyn ByteStore,
        node: NodeId,
        offset: u64,
        out: &mut [u8],
    ) -> Result<usize, FileSystemError>;

    /// Кладёт `data` по смещению `offset` и возвращает число записанных байт. Промежуток между
    /// прежней длиной файла и началом записи заполняется нулями. Если запись выводит файл
    /// за предел, установленный файловой системой, возвращает `NoSpace` и содержимое узла
    /// оставляет как есть. Если узел оказался каталогом, возвращает `IsADirectory`.
    fn write_at(
        &mut self,
        stores: &mut dyn ByteStore,
        node: NodeId,
        offset: u64,
        data: &[u8],
    ) -> Result<usize, FileSystemError>;

    /// Дописывает `data` в конец файла и возвращает смещение, по которому легли байты. Если
    /// дозапись выводит файл за предел, установленный файловой системой, возвращает `NoSpace`.
    /// Если узел оказался каталогом, возвращает `IsADirectory`.
    fn append(
        &mut self,
        stores: &mut dyn ByteStore,
        node: NodeId,
        data: &[u8],
    ) -> Result<u64, FileSystemError>;

    /// Задаёт длину файла: байты за `size` отбрасываются, недостающие дописываются нулями. Если
    /// `size` больше предела, установленного файловой системой, возвращает `NoSpace`. Если узел
    /// оказался каталогом, возвращает `IsADirectory`.
    fn set_size(
        &mut self,
        stores: &mut dyn ByteStore,
        node: NodeId,
        size: u64,
    ) -> Result<(), FileSystemError>;

    /// Укладывает записи каталога подряд в начало `out`, начиная с позиции `from`, пока они
    /// помещаются, и возвращает итог перечисления: число уложенных записей, позицию продолжения и
    /// признак исчерпанного каталога. `from` - [`DirCursor::START`] либо позиция, выданная
    /// прежним перечислением этого же узла, а трактовка значения принадлежит бекенду. Нулевое число
    /// записей значит, что буфер остался нетронутым. Если узел оказался файлом, возвращает
    /// `NotADirectory`.
    fn list(
        &self,
        node: NodeId,
        from: DirCursor,
        out: &mut [u8],
    ) -> Result<Listed, FileSystemError>;

    /// Возвращает окно байт узла, при необходимости перенося их в отдельное хранилище. Если узел
    /// байт хранилища не занимает, возвращает `NotSupported`.
    fn backing(
        &mut self,
        stores: &mut dyn ByteStore,
        node: NodeId,
    ) -> Result<Window, FileSystemError>;

    /// Закрепляет ёмкость узла после выдачи клиенту capability его хранилища.
    fn pin(&mut self, node: NodeId) -> Result<(), FileSystemError>;

    /// Заводит файл поверх уже существующего хранилища. Бекенд без такой формы отвечает
    /// `NotSupported`.
    fn create_backed(
        &mut self,
        _path: Path<'_>,
        _store: StoreId,
        _size: u64,
        _capacity: u64,
    ) -> Result<NodeId, FileSystemError> {
        Err(FileSystemError::NotSupported)
    }
}
