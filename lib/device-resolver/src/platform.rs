use syscall::IrqTrigger;

/// Контроллер прерываний, в пространстве которого нумеруется линия. Платформа с
/// единственным контроллером пользуется [`ControllerId::PLATFORM`].
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ControllerId(u16);

/// Линия прерывания сочетает номер и контроллер, которому он принадлежит.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct InterruptLine {
    controller: ControllerId,
    number: u16,
}

/// Линия прерывания устройства вместе с триггером из описания платформы.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct DeviceInterrupt {
    /// Линия, которую минтит держатель полномочия.
    pub line: InterruptLine,
    /// Триггер, которым контроллер настраивает линию.
    pub trigger: IrqTrigger,
}

/// MMIO-окно устройства в физическом адресном пространстве.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct DeviceMmio {
    /// Базовый физический адрес окна.
    pub base: u64,
    /// Размер окна в байтах.
    pub size: u64,
}

/// Селектор устройства: совместимость и номер экземпляра среди совместимых.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct DeviceSelector<'a> {
    /// Строка совместимости устройства.
    pub compatible: &'a str,
    /// Номер экземпляра среди устройств той же совместимости.
    pub instance: u16,
}

/// Отказ разбора описания платформы.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ResolveError {
    /// Описание не содержит устройства, названного селектором.
    DeviceNotFound,
    /// Устройство описано без линий прерываний.
    NoInterrupts,
    /// Устройство описано без MMIO-окон.
    NoMmio,
    /// У устройства нет ресурса с таким номером.
    ResourceOutOfRange,
    /// Контроллер прерываний устройства реестру биндингов неизвестен.
    UnknownController,
    /// Окно устройства лежит за шиной без `ranges` либо вне записей её `ranges` и не
    /// имеет физического адреса.
    Untranslatable,
    /// Описание устройства не разбирается: битые ячейки либо отсутствующие
    /// свойства контроллера.
    Malformed,
    /// Ядро не выдало capability на окно устройства.
    MmioUnavailable,
}

/// Описание платформы, отвечающее по селектору устройства. Формат описания -
/// деталь реализации.
pub trait PlatformDescription {
    /// Линия прерывания `index` устройства `device` вместе с её триггером.
    fn interrupt(
        &self,
        device: DeviceSelector<'_>,
        index: u16,
    ) -> Result<DeviceInterrupt, ResolveError>;

    /// MMIO-окно `index` устройства `device`.
    fn mmio(&self, device: DeviceSelector<'_>, index: u16) -> Result<DeviceMmio, ResolveError>;
}

impl ControllerId {
    /// Корневой контроллер прерываний платформы.
    pub const PLATFORM: Self = Self(0);

    /// Порядковый номер контроллера.
    #[must_use]
    pub const fn index(self) -> u16 {
        self.0
    }
}

impl InterruptLine {
    /// Собирает линию `number` в пространстве контроллера `controller`.
    #[must_use]
    pub const fn new(controller: ControllerId, number: u16) -> Self {
        Self { controller, number }
    }

    /// Контроллер, которому принадлежит номер линии.
    #[must_use]
    pub const fn controller(self) -> ControllerId {
        self.controller
    }

    /// Номер линии в пространстве её контроллера.
    #[must_use]
    pub const fn number(self) -> u16 {
        self.number
    }
}
