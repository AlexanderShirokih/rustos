//! Подключение фасада к системе: capability становится транспортом и отображением.

use filesystem_abi::FileSystemError;
use ipc::{ReceivedResources, Transport};
use syscall::{ObjectKind, kind};

/// Источник транспорта и отображений, на котором работает фасад. Окружение владеет
/// capability, которые получило, и закрывает их вместе с собой.
pub trait Environment {
    /// Владеющая capability принятого ответа для вида объекта `K`.
    type Handle<K: ObjectKind>;

    /// Транспорт, по которому идут вызовы контракта файловой системы. Виды его принятых
    /// ресурсов совпадают с видами окружения.
    type Transport: Transport<
        Resources: ReceivedResources<
            Handle<kind::Port> = Self::Handle<kind::Port>,
            Handle<kind::MemoryRegion> = Self::Handle<kind::MemoryRegion>,
        >,
    >;

    /// Отображение буфера данных сессии.
    type Region: SharedRegion;

    /// Открывает транспорт поверх capability сессии файловой системы.
    fn connect(&self, port: Self::Handle<kind::Port>) -> Result<Self::Transport, FileSystemError>;

    /// Отображает capability региона в адресное пространство программы.
    fn map(
        &self,
        region: Self::Handle<kind::MemoryRegion>,
    ) -> Result<Self::Region, FileSystemError>;
}

/// Память, разделённая с сервером. Доступ к ней идёт копированием.
pub trait SharedRegion {
    /// Размер отображения в байтах.
    fn size(&self) -> usize;

    /// Копирует из региона со смещения `offset` столько байт, сколько вмещает `dst`.
    fn read_at(&self, offset: usize, dst: &mut [u8]) -> Result<(), FileSystemError>;

    /// Копирует `src` в регион по смещению `offset`.
    fn write_at(&self, offset: usize, src: &[u8]) -> Result<(), FileSystemError>;
}
