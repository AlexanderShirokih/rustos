//! Окружение фасада. Capability превращается в транспорт контракта и в отображённый регион.

use filesystem_abi::FileSystemError;
use ipc::{Transport, wire::Cap};

/// Владелец capability и отображений, поверх которого работает фасад. Переданная capability
/// переходит реализации, и закрытие её на отказе - тоже её дело.
pub trait Environment {
    /// Транспорт контракта.
    type Transport: Transport;

    /// Отображённый регион, разделённый с сервером.
    type Region: SharedRegion;

    /// Транспорт поверх capability, которую отдал `Namespace::resolve`.
    fn connect(&self, port: Cap) -> Result<Self::Transport, FileSystemError>;

    /// Отображение региона, который отдала операция `FileSystem::acquire_buffer`.
    fn map(&self, region: Cap) -> Result<Self::Region, FileSystemError>;
}

/// Регион, страницы которого разделены с сервером. Доступ к ним идёт копированием.
pub trait SharedRegion {
    /// Размер отображения в байтах.
    fn size(&self) -> usize;

    /// Копирует из региона со смещения `offset` столько байт, сколько вмещает `dst`.
    fn read_at(&self, offset: usize, dst: &mut [u8]) -> Result<(), FileSystemError>;

    /// Копирует `src` в регион по смещению `offset`.
    fn write_at(&self, offset: usize, src: &[u8]) -> Result<(), FileSystemError>;
}
