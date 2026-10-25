//! Подключение фасада к системе: capability становится транспортом и отображением.

use filesystem_abi::{FileSystemError, IoRecord, file_system};
use ipc::{Channel, ReceivedResources, Timeout};
use syscall::{ObjectKind, kind};

/// Источник транспорта и отображений, на котором работает фасад. Окружение владеет
/// capability, которые получило, и закрывает их вместе с собой.
pub trait Environment: Sized {
    /// Владеющая capability принятого ответа для вида объекта `K`.
    type Handle<K: ObjectKind>;

    /// Транспорт, по которому идут вызовы контракта файловой системы. Виды его принятых
    /// ресурсов совпадают с видами окружения.
    type Transport: Channel<
        Resources: ReceivedResources<
            Handle<kind::Port> = Self::Handle<kind::Port>,
            Handle<kind::MemoryRegion> = Self::Handle<kind::MemoryRegion>,
            Handle<kind::Signal> = Self::Handle<kind::Signal>,
        >,
    >;

    /// Отображение файлового диапазона.
    type Region: SharedRegion;

    /// Контекст отображений и единственного обмена сессии, владеющий необходимым окружением.
    type Io: IoContext<Self>;

    /// Создаёт контекст, живущий независимо от фасада FileSystem.
    fn io(&self) -> Self::Io;

    /// Открывает транспорт поверх принятой сессии файловой системы.
    fn connect(
        &self,
        session: ipc::wire::Session<file_system::Protocol, Self::Handle<kind::Port>>,
    ) -> Result<Self::Transport, FileSystemError>;
}

/// Адаптер файлового обмена к окружению. Capability принимает управляющий клиент,
/// exchange передаёт только управляющие записи, map сохраняет режим доступа.
pub trait IoContext<E: Environment> {
    /// Передаёт запись и ждёт ответ той же операции. Отказ после публикации не допускает replay.
    fn exchange(
        &mut self,
        client: &file_system::Client<E::Transport>,
        request: IoRecord,
        wait: Timeout,
    ) -> crate::Result<IoRecord>;

    /// Отображённый scratch обмена. Первое обращение устанавливает обмен.
    fn scratch(&mut self, client: &file_system::Client<E::Transport>) -> crate::Result<&E::Region>;

    /// Отображает зарегистрированный регион с правами, разрешёнными сервером.
    fn map(
        &self,
        region: E::Handle<kind::MemoryRegion>,
        writable: bool,
    ) -> Result<E::Region, FileSystemError>;
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
