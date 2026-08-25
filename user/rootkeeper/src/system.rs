//! Композиция, которую rootkeeper поднимает при загрузке, и её supervisor.

use alloc::{boxed::Box, format, vec::Vec};
use core::{
    fmt::{Arguments, Write as _},
    num::NonZeroU64,
};

use bootstrap_abi::bootstrap;
use component_abi::{SESSION_SCOPE_MAX, component_sessions};
use device_resolver::{
    DeviceMmio, DeviceSelector, DeviceTreePlatform, PlatformDescription, ResolveError, acquire_mmio,
};
use device_resolver_aarch64::PLATFORM_BINDINGS;
use fdt::devicetree::DeviceTree;
use ipc::wire::Str;
use log_abi::{LogBuffer, log};
use managed_component::{
    ComponentRoots, ComponentSlots, ManagedComponent, ManagedStartupError, launch_component,
};
use runtime::{
    BorrowedHandle, IrqControl, MemoryRegion, OwnedHandle, PortTransport, ReadOnlyMapping,
    Resource, Timeout, wait_any,
};
use supervisor_abi::ExtendError;
use syscall::{Handle, Rights, SIGNALED, SyscallError, WAIT_MANY_MAX_COUNT, WaitItem};
use userland_image::{
    BOOT_PLAN_PATH, BootPlan, COMPONENT_PROGRAM_NAME, DecodedImage, DecodedProgram, OnExhausted,
    PlanComponent, PlanDecodeError, PlanDevice, SlotSource, decode, decode_plan, decode_program,
};

use crate::{
    extension::{Answer, ExtensionPort, Request},
    relay,
};

const LOG_TAG: &str = "supervisor";

/// Конечный срок остановки компонента.
pub const STOP_TIMEOUT: Timeout = Timeout::from_secs(5);

const SUPERVISOR_WAITS: usize = 2;

const WHOLE_EXPORT: &str = "";

/// Источник делегированных лог-сессий компонентов.
pub type LogSessions = fn(NonZeroU64) -> Option<OwnedHandle>;

/// Исход прохода supervisor: этим кодом ядро гасит машину.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Stopped {
    /// Инфраструктура supervisor не поднялась.
    Startup = 1,
    /// Userland-образ либо план загрузки недоступны.
    BadImage = 2,
    /// Критический компонент завершился.
    Subsystem = 3,
    /// Ресурс устройства, названный планом, недоступен композиции.
    Device = 4,
}

/// Причина, по которой композиция не поднялась.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum BootFailure {
    /// Лог-сессия компонента не выдана.
    LogSession,
    /// Userland-образ недоступен либо не разобран.
    Image,
    /// В образе нет записи плана загрузочной композиции.
    PlanMissing,
    /// Запись плана не разобрана.
    Plan(PlanDecodeError),
    /// Зависимости плана не укладываются в порядок запуска.
    PlanOrder,
    /// Запись компонента, поданная транзакцией расширения, не разобрана.
    Record,
    /// Имя экземпляра занято другим компонентом композиции.
    Instance,
    /// В образе нет программы компонента.
    ProgramMissing,
    /// Корневой metering resource недоступен.
    Resource,
    /// Резерв бюджета компонента не выделен либо композиция не помещается в набор ожидания.
    Capacity,
    /// Полномочия провижининга устройств недоступны.
    DeviceAuthority,
    /// Описание платформы не дало ресурс, названный слотом.
    Device(ResolveError),
    /// Ресурс устройства уже отдан другому компоненту композиции.
    DeviceBusy,
    /// Провайдер сервисного слота не поднят.
    Provider,
    /// Провайдер не установил сессию сервисного слота.
    Session,
    /// Провайдер не установил сессию, суженную до каталога компонента.
    Directory,
    /// Capability слота не сминчена.
    Slot,
    /// Managed-запуск компонента не завершился.
    Launch(ManagedStartupError),
}

/// Исход одного оборота supervisor.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Supervised {
    /// Компонент ушёл с кодом `exit_code`, и композиция подняла его новую
    /// incarnation вместе с `dependents` его потребителями.
    Restarted {
        exit_code: Option<u32>,
        dependents: u16,
    },
    /// Некритический компонент исчерпал запуски и снят с композиции вместе с
    /// `dependents` его потребителями, а сама композиция осталась поднятой.
    Abandoned {
        exit_code: Option<u32>,
        dependents: u16,
    },
    /// Оборот обслужил операцию транзакции расширения композиции.
    Extended,
    /// Композиция остановлена: восстановить её оборот supervisor не смог.
    Stopped(Stopped),
}

/// Userland-образ, отображённый в адресное пространство rootkeeper.
pub struct SystemImage {
    region: MemoryRegion,
    mapping: ReadOnlyMapping,
}

struct FdtMapping {
    mapping: ReadOnlyMapping,
    offset: usize,
}

impl FdtMapping {
    fn blob(&self) -> Option<&[u8]> {
        self.mapping.as_bytes().get(self.offset..)
    }
}

/// Определение компонента транзакцией расширения: запись плана с одним компонентом и регион,
/// чьи байты начинаются его программой.
pub struct Definition {
    record: Box<[u8]>,
    program: MemoryRegion,
}

struct DeviceAuthority {
    description: Option<FdtMapping>,
    _region: Option<MemoryRegion>,
    lines: Option<IrqControl>,
    windows: Vec<GrantedWindow>,
}

struct GrantedWindow {
    window: DeviceMmio,
    owner: u16,
}

enum ComponentRecord<'a> {
    Booted(PlanComponent<'a>),
    Defined {
        record: Box<[u8]>,
        mapping: ReadOnlyMapping,
        _region: MemoryRegion,
    },
}

/// Компонент композиции вместе с его записью плана.
pub struct Component<'a> {
    managed: ManagedComponent<ComponentRoots>,
    record: ComponentRecord<'a>,
    log: OwnedHandle,
    owner: Option<NonZeroU64>,
    charged_launches: u32,
}

struct Provider<'a> {
    roots: &'a ComponentRoots,
    plan: PlanComponent<'a>,
}

/// Поднятая композиция rootkeeper.
pub struct RootSystem<'a> {
    components: Vec<Component<'a>>,
    order: Vec<u16>,
    devices: DeviceAuthority,
    extension: Option<ExtensionPort>,
    kernel: Handle,
    image: &'a SystemImage,
    entries: DecodedImage<'a>,
    resource: Resource,
    log_sessions: LogSessions,
}

/// Код, которым остановка композиции завершает её компоненты.
pub const SHUTDOWN_EXIT_CODE: u32 = 1;
const EXIT_MESSAGE_MAX: usize = 96;

impl From<Stopped> for u64 {
    fn from(stopped: Stopped) -> Self {
        stopped as Self
    }
}

impl BootFailure {
    /// Код выхода supervisor, соответствующий отказу.
    #[must_use]
    pub const fn stopped(self) -> Stopped {
        match self {
            Self::Image
            | Self::PlanMissing
            | Self::Plan(_)
            | Self::PlanOrder
            | Self::ProgramMissing => Stopped::BadImage,
            Self::DeviceAuthority | Self::Device(_) | Self::DeviceBusy => Stopped::Device,
            Self::LogSession
            | Self::Record
            | Self::Instance
            | Self::Resource
            | Self::Capacity
            | Self::Provider
            | Self::Session
            | Self::Directory
            | Self::Slot
            | Self::Launch(_) => Stopped::Startup,
        }
    }

    /// Отказ операции транзакции расширения, соответствующий отказу подъёма.
    #[must_use]
    pub const fn refused(self) -> ExtendError {
        match self {
            Self::Image | Self::PlanMissing | Self::Plan(_) | Self::Record => {
                ExtendError::BadRecord
            }
            Self::Instance => ExtendError::Duplicate,
            Self::PlanOrder | Self::Provider => ExtendError::Unresolved,
            Self::LogSession | Self::Resource | Self::Capacity => ExtendError::NoSpace,
            Self::ProgramMissing
            | Self::DeviceAuthority
            | Self::Device(_)
            | Self::DeviceBusy
            | Self::Session
            | Self::Directory
            | Self::Slot
            | Self::Launch(_) => ExtendError::NotStarted,
        }
    }

    /// Строка отказа для журнала.
    #[must_use]
    pub const fn message(self) -> &'static str {
        match self {
            Self::LogSession => "log session not minted",
            Self::Image => "userland image unavailable",
            Self::PlanMissing => "boot plan record missing",
            Self::Plan(_) => "boot plan not decoded",
            Self::PlanOrder => "boot plan dependencies have no start order",
            Self::Record => "component record not decoded",
            Self::Instance => "instance name already taken",
            Self::ProgramMissing => "component program missing",
            Self::Resource => "root resource unavailable",
            Self::Capacity => "component reserve not granted",
            Self::DeviceAuthority => "platform description unavailable",
            Self::Device(_) => "device not described by the platform",
            Self::DeviceBusy => "device resource taken by another component",
            Self::Provider => "service provider not running",
            Self::Session => "service provider did not establish a session",
            Self::Directory => "component directory not granted",
            Self::Slot => "component slot not minted",
            Self::Launch(_) => "component not started",
        }
    }
}

impl SystemImage {
    /// Забирает у ядра capability образа и отображает его read-only.
    pub fn acquire(kernel: Handle) -> Result<Self, BootFailure> {
        let client = bootstrap::Client::new(PortTransport::client(kernel));
        let cap = client
            .acquire_userland_image()
            .ok()
            .and_then(Result::ok)
            .ok_or(BootFailure::Image)?;
        let region = MemoryRegion::adopt(cap);
        let mapping = region
            .map_full_read_only()
            .map_err(|_| BootFailure::Image)?;
        Ok(Self { region, mapping })
    }

    /// Таблица записей образа.
    pub fn entries(&self) -> Result<DecodedImage<'_>, BootFailure> {
        decode(self.mapping.as_bytes()).map_err(|_| BootFailure::Image)
    }

    /// Разбирает план загрузочной композиции из записи `entry`.
    pub fn plan(&self, entry: &str) -> Result<BootPlan<'_>, BootFailure> {
        let record = self
            .entries()?
            .entry(entry)
            .ok_or(BootFailure::PlanMissing)?;
        decode_plan(record.data()).map_err(BootFailure::Plan)
    }

    /// Регион образа, из которого нарезаются производные капы incarnation.
    #[must_use]
    pub const fn region(&self) -> &MemoryRegion {
        &self.region
    }
}

impl Definition {
    /// Копирует запись плана и берёт регион её программы во владение.
    #[must_use]
    pub fn new(record: &[u8], program: MemoryRegion) -> Self {
        Self {
            record: Box::from(record),
            program,
        }
    }
}

impl DeviceAuthority {
    const fn absent() -> Self {
        Self {
            description: None,
            _region: None,
            lines: None,
            windows: Vec::new(),
        }
    }

    const fn is_absent(&self) -> bool {
        self.description.is_none()
    }

    fn acquire(kernel: Handle) -> Result<Self, BootFailure> {
        let client = bootstrap::Client::new(PortTransport::client(kernel));
        let fdt = client
            .acquire_boot_fdt()
            .ok()
            .and_then(Result::ok)
            .ok_or(BootFailure::DeviceAuthority)?;
        let region = MemoryRegion::adopt(fdt.region);
        let description = FdtMapping {
            mapping: region
                .map_full_read_only()
                .map_err(|_| BootFailure::DeviceAuthority)?,
            offset: fdt.offset as usize,
        };
        let lines = client
            .acquire_irq_control()
            .ok()
            .and_then(Result::ok)
            .ok_or(BootFailure::DeviceAuthority)?;
        Ok(Self {
            description: Some(description),
            _region: Some(region),
            lines: Some(IrqControl::from_handle(OwnedHandle::adopt(lines))),
            windows: Vec::new(),
        })
    }

    fn resolve<R>(
        &self,
        body: impl FnOnce(&DeviceTreePlatform<'_>) -> Result<R, ResolveError>,
    ) -> Result<R, BootFailure> {
        let blob = self
            .description
            .as_ref()
            .and_then(FdtMapping::blob)
            .ok_or(BootFailure::DeviceAuthority)?;
        let tree = DeviceTree::from_bytes(blob).map_err(|_| BootFailure::DeviceAuthority)?;
        body(&DeviceTreePlatform::new(&tree, PLATFORM_BINDINGS)).map_err(BootFailure::Device)
    }

    fn line(
        &self,
        device: PlanDevice<'_>,
        owner: &str,
        kernel_log: &bootstrap::Client<PortTransport>,
    ) -> Result<OwnedHandle, BootFailure> {
        let interrupt = self.first_match(device, |platform, selector| {
            platform.interrupt(selector, device.resource)
        })?;
        let lines = self.lines.as_ref().ok_or(BootFailure::DeviceAuthority)?;
        let number = interrupt.line.number();
        match lines.mint(number, interrupt.trigger) {
            Ok(line) => Ok(line.into_handle()),
            Err(runtime::Error::Syscall(SyscallError::AlreadyBound)) => {
                // Линия принадлежит одному компоненту: занявший её держит
                // capability, и второй bind ядро отвергает.
                log_device_conflict(kernel_log, owner, format_args!("irq line {number}"));
                Err(BootFailure::DeviceBusy)
            }
            Err(_) => Err(BootFailure::Slot),
        }
    }

    fn window(
        &mut self,
        kernel: Handle,
        device: PlanDevice<'_>,
        owner: u16,
        name: &str,
        kernel_log: &bootstrap::Client<PortTransport>,
    ) -> Result<OwnedHandle, BootFailure> {
        let window = self.first_match(device, |platform, selector| {
            platform.mmio(selector, device.resource)
        })?;
        if self
            .windows
            .iter()
            .any(|granted| granted.window == window && granted.owner != owner)
        {
            let base = window.base;
            log_device_conflict(kernel_log, name, format_args!("mmio window {base:#x}"));
            return Err(BootFailure::DeviceBusy);
        }
        let client = bootstrap::Client::new(PortTransport::client(kernel));
        let region: MemoryRegion = acquire_mmio(&client, window).map_err(BootFailure::Device)?;
        // Смена incarnation забирает окно заново, и запись владельца остаётся
        // одна: учёт ведётся по компоненту плана, а не по его запуску.
        if !self
            .windows
            .iter()
            .any(|granted| granted.window == window && granted.owner == owner)
        {
            self.windows.push(GrantedWindow { window, owner });
        }
        Ok(region.into_handle())
    }

    fn first_match<R>(
        &self,
        device: PlanDevice<'_>,
        body: impl Fn(&DeviceTreePlatform<'_>, DeviceSelector<'_>) -> Result<R, ResolveError> + Copy,
    ) -> Result<R, BootFailure> {
        // Драйвер объявил семейство: описание платформы решает, какая из его
        // совместимостей стоит на этой машине. Причину отказа несёт последняя.
        let mut failure = BootFailure::Device(ResolveError::DeviceNotFound);
        for compatible in device.compatible() {
            let selector = DeviceSelector {
                compatible,
                instance: device.instance,
            };
            match self.resolve(|platform| body(platform, selector)) {
                Ok(resolved) => return Ok(resolved),
                Err(error) => failure = error,
            }
        }
        Err(failure)
    }
}

impl<'a> ComponentRecord<'a> {
    fn defined(definition: Definition) -> Result<Self, BootFailure> {
        let Definition { record, program } = definition;
        // Загрузчик читает сегменты прямо из региона, и его страницы обязаны быть неизменны:
        // маска доступа с записью отвергается отображением.
        let mapping = program
            .map_full_read_only()
            .map_err(|_| BootFailure::Record)?;
        Ok(Self::Defined {
            record,
            mapping,
            _region: program,
        })
    }

    fn plan(&self) -> Result<PlanComponent<'_>, BootFailure> {
        match self {
            Self::Booted(plan) => Ok(*plan),
            Self::Defined { record, .. } => decode_plan(record)
                .map_err(|_| BootFailure::Record)?
                .component(0)
                .ok_or(BootFailure::Record),
        }
    }

    fn program(&self, entries: &DecodedImage<'a>) -> Result<DecodedProgram<'_>, BootFailure> {
        let plan = self.plan()?;
        match self {
            Self::Booted(_) => {
                let record = format!("{}/{COMPONENT_PROGRAM_NAME}", plan.executable());
                entries
                    .entry(&record)
                    .and_then(|entry| entry.program().ok())
                    .ok_or(BootFailure::ProgramMissing)
            }
            Self::Defined { mapping, .. } => decode_program(plan.executable(), mapping.as_bytes())
                .map_err(|_| BootFailure::ProgramMissing),
        }
    }
}

impl Component<'_> {
    /// Managed-примитив компонента.
    #[must_use]
    pub const fn managed(&self) -> &ManagedComponent<ComponentRoots> {
        &self.managed
    }
}

impl<'a> Provider<'a> {
    fn of(components: &'a [Component<'_>], instance: &str) -> Result<Self, BootFailure> {
        let index = find_instance(components, instance).ok_or(BootFailure::Provider)?;
        let component = components
            .get(usize::from(index))
            .ok_or(BootFailure::Provider)?;
        Ok(Self {
            roots: component
                .managed
                .running()
                .ok_or(BootFailure::Provider)?
                .roots(),
            plan: component.record.plan()?,
        })
    }
}

impl<'a> RootSystem<'a> {
    /// Поднимает компоненты плана `plan` в топологическом порядке.
    pub fn bring_up(
        image: &'a SystemImage,
        plan: BootPlan<'a>,
        kernel: Handle,
        log_sessions: LogSessions,
    ) -> Result<Self, BootFailure> {
        let entries = image.entries()?;
        let resource = Resource::self_resource().map_err(|_| BootFailure::Resource)?;
        let mut system = Self {
            components: Vec::with_capacity(usize::from(plan.len())),
            order: Vec::new(),
            devices: DeviceAuthority::absent(),
            extension: None,
            kernel,
            image,
            entries,
            resource,
            log_sessions,
        };
        if let Err(failure) = system.compose(plan) {
            // Поднятые компоненты уходят вместе с загрузкой: иначе они держат
            // линии и окна устройств за композицией, которой нет.
            system.shutdown();
            return Err(failure);
        }
        Ok(system)
    }

    /// Компонент с номером `index` в наборе композиции.
    #[must_use]
    pub fn component(&self, index: u16) -> Option<&Component<'a>> {
        self.components.get(usize::from(index))
    }

    /// Номер компонента с именем экземпляра `instance`.
    #[must_use]
    pub fn instance(&self, instance: &str) -> Option<u16> {
        find_instance(&self.components, instance)
    }

    /// Номера компонентов в порядке запуска. Остановка идёт в обратном.
    #[must_use]
    pub fn start_order(&self) -> &[u16] {
        &self.order
    }

    /// Поднимает набор транзакции расширения в топологическом порядке по объединению рёбер
    /// набора и уже поднятых компонентов. Отказ любого шага снимает набор целиком, оставляя
    /// поднятые компоненты нетронутыми.
    pub fn extend(
        &mut self,
        caller: NonZeroU64,
        definitions: Vec<Definition>,
    ) -> Result<(), BootFailure> {
        let first = self.components.len();
        let order = self.order.clone();
        if let Err(failure) = self.compose_extension(caller, definitions) {
            self.discard(first, order);
            return Err(failure);
        }
        Ok(())
    }

    /// Сессия компонента `instance` для вызывателя со значком `caller`. Компонент, которого
    /// вызыватель не определял, недоступен.
    pub fn caller_session(
        &self,
        caller: NonZeroU64,
        instance: &str,
    ) -> Result<OwnedHandle, BootFailure> {
        let index = find_instance(&self.components, instance).ok_or(BootFailure::Provider)?;
        let defined = self
            .component(index)
            .is_some_and(|component| component.owner == Some(caller));
        if !defined {
            return Err(BootFailure::Provider);
        }
        session(&self.components, instance, caller)
    }

    /// Оборот supervisor: ждёт завершения любого компонента, потока
    /// `relay_thread` либо операции транзакции расширения, записывает исход в
    /// журнал ядра и поднимает ушедшему компоненту смену.
    ///
    /// Завершение потока пересылки критично: восстановить его композиция не
    /// умеет.
    pub fn supervise_turn(&mut self, relay_thread: Handle) -> Supervised {
        let kernel_log = bootstrap::Client::new(PortTransport::client(self.kernel));
        let mut waits = Vec::with_capacity(self.components.len() + SUPERVISOR_WAITS);
        let mut watched: Vec<u16> = Vec::with_capacity(self.components.len());
        for index in 0..self.component_count() {
            let Some(running) = self.component(index).and_then(|c| c.managed.running()) else {
                continue;
            };
            waits.push(WaitItem::new(running.process().handle().as_raw(), SIGNALED));
            watched.push(index);
        }
        waits.push(WaitItem::new(relay_thread, SIGNALED));
        if let Some(posted) = self.extension.as_ref().map(ExtensionPort::posted) {
            waits.push(WaitItem::new(posted, SIGNALED));
        }

        match wait_any(&waits, Timeout::INFINITE) {
            Ok(index) if index < watched.len() => {
                self.replace_component(watched[index], &kernel_log)
            }
            Ok(index) if index == watched.len() => {
                log_exit(&kernel_log, "log relay", relay::exit_code());
                Supervised::Stopped(Stopped::Subsystem)
            }
            Ok(_) => self.serve_extension(),
            Err(_) => {
                let _ = kernel_log.log_str(LOG_TAG, "supervisor wait failed");
                Supervised::Stopped(Stopped::Subsystem)
            }
        }
    }

    /// Завершает компоненты композиции в порядке, обратном порядку запуска.
    pub fn shutdown(&mut self) {
        if let Some(extension) = &mut self.extension {
            extension.shutdown();
        }
        self.stop_from(0);
    }

    fn compose(&mut self, plan: BootPlan<'a>) -> Result<(), BootFailure> {
        if plan_needs_devices(&plan) {
            self.devices = DeviceAuthority::acquire(self.kernel)?;
        }
        self.extension = Some(ExtensionPort::start().map_err(|_| BootFailure::Slot)?);
        for index in 0..plan.len() {
            let component = plan.component(index).ok_or(BootFailure::PlanOrder)?;
            let log = (self.log_sessions)(relay::component_badge(index))
                .ok_or(BootFailure::LogSession)?;
            let managed = ManagedComponent::new(&self.resource, component.memory_pages())
                .map_err(|_| BootFailure::Capacity)?;
            self.components.push(Component {
                managed,
                record: ComponentRecord::Booted(component),
                log,
                owner: None,
                charged_launches: 1,
            });
        }
        self.publish_names();
        self.order = start_order(&self.components)?;
        for index in self.order.clone() {
            self.launch(index)?;
        }
        Ok(())
    }

    fn compose_extension(
        &mut self,
        caller: NonZeroU64,
        definitions: Vec<Definition>,
    ) -> Result<(), BootFailure> {
        let first = self.components.len();
        if first + definitions.len() + SUPERVISOR_WAITS > WAIT_MANY_MAX_COUNT {
            return Err(BootFailure::Capacity);
        }
        for definition in definitions {
            let index = u16::try_from(self.components.len()).map_err(|_| BootFailure::Capacity)?;
            let record = ComponentRecord::defined(definition)?;
            let pages = {
                let plan = record.plan()?;
                if find_instance(&self.components, plan.name()).is_some() {
                    return Err(BootFailure::Instance);
                }
                plan.memory_pages()
            };
            let log = (self.log_sessions)(relay::component_badge(index))
                .ok_or(BootFailure::LogSession)?;
            let managed =
                ManagedComponent::new(&self.resource, pages).map_err(|_| BootFailure::Capacity)?;
            self.components.push(Component {
                managed,
                record,
                log,
                owner: Some(caller),
                charged_launches: 1,
            });
        }
        self.publish_names();
        if self.devices.is_absent() && self.needs_devices(first) {
            self.devices = DeviceAuthority::acquire(self.kernel)?;
        }
        self.order = start_order(&self.components)?;
        for index in self.order.clone() {
            if usize::from(index) >= first {
                self.launch(index)?;
            }
        }
        Ok(())
    }

    fn needs_devices(&self, first: usize) -> bool {
        self.components[first..].iter().any(|component| {
            component.record.plan().is_ok_and(|plan| {
                plan.slots()
                    .any(|slot| matches!(slot, SlotSource::Irq(_) | SlotSource::Mmio(_)))
            })
        })
    }

    fn discard(&mut self, first: usize, order: Vec<u16>) {
        self.stop_from(first);
        self.components.truncate(first);
        self.publish_names();
        self.order = order;
    }

    fn publish_names(&self) {
        let names: Vec<&str> = self.components.iter().map(instance_name).collect();
        relay::install_names(names.iter().copied());
    }

    fn stop_from(&mut self, first: usize) {
        for index in self.order.clone().into_iter().rev() {
            if usize::from(index) >= first {
                self.stop(index);
            }
        }
        // Компонент, до запуска которого подъём не дошёл, в порядке запуска не стоит.
        for index in (first..self.components.len()).rev() {
            if let Ok(index) = u16::try_from(index) {
                self.stop(index);
            }
        }
    }

    fn stop(&mut self, index: u16) {
        if let Some(component) = self.components.get_mut(usize::from(index)) {
            let _ = component.managed.stop(SHUTDOWN_EXIT_CODE, STOP_TIMEOUT);
        }
    }

    fn serve_extension(&mut self) -> Supervised {
        let Some(request) = self
            .extension
            .as_ref()
            .and_then(ExtensionPort::take_request)
        else {
            return Supervised::Extended;
        };
        let answer = match request {
            Request::Commit {
                caller,
                definitions,
            } => Answer::Extended(
                self.extend(caller, definitions)
                    .map_err(BootFailure::refused),
            ),
            Request::Session { caller, instance } => Answer::Session(
                self.caller_session(caller, &instance)
                    .map_err(refused_session),
            ),
        };
        if let Some(extension) = &self.extension {
            extension.answer(answer);
        }
        Supervised::Extended
    }

    fn launch(&mut self, index: u16) -> Result<(), BootFailure> {
        let slots = self.provision(index)?;
        let Self {
            components,
            entries,
            ..
        } = self;
        let component = components
            .get_mut(usize::from(index))
            .ok_or(BootFailure::PlanOrder)?;
        let Component {
            managed,
            record,
            log,
            ..
        } = component;
        let plan = record.plan()?;
        let program = record.program(entries)?;
        launch_component(
            managed,
            &program,
            plan.name(),
            log,
            plan.startup_timeout(),
            slots,
        )
        .map_err(BootFailure::Launch)
    }

    fn provision(&mut self, index: u16) -> Result<ComponentSlots, BootFailure> {
        let badge = relay::component_badge(index);
        let kernel_log = bootstrap::Client::new(PortTransport::client(self.kernel));
        let Self {
            components,
            devices,
            extension,
            kernel,
            image,
            ..
        } = self;
        let plan = components
            .get(usize::from(index))
            .ok_or(BootFailure::PlanOrder)?
            .record
            .plan()?;
        let mut slots = ComponentSlots::default();
        for source in plan.slots() {
            let capability = match source {
                SlotSource::Userland => image
                    .region()
                    .handle()
                    .duplicate(Rights::READ | Rights::TRANSFER | Rights::DUPLICATE, None)
                    .map_err(|_| BootFailure::Slot)?,
                SlotSource::Irq(device) => devices.line(device, plan.name(), &kernel_log)?,
                SlotSource::Mmio(device) => {
                    devices.window(*kernel, device, index, plan.name(), &kernel_log)?
                }
                SlotSource::Service { provider } => session(components, provider, badge)?,
                // Область слота-каталога назначает supervisor: путь берёт запись программы.
                SlotSource::Directory { provider } => {
                    component_area(components, provider, plan.executable())?
                }
                SlotSource::Supervisor => extension
                    .as_ref()
                    .and_then(|port| port.slot(badge))
                    .ok_or(BootFailure::Slot)?,
                SlotSource::Control { provider } => control_session(components, provider, badge)?,
            };
            slots.push(capability).map_err(BootFailure::Launch)?;
        }
        Ok(slots)
    }

    fn component_count(&self) -> u16 {
        u16::try_from(self.components.len()).unwrap_or(u16::MAX)
    }

    fn replace_component(
        &mut self,
        index: u16,
        kernel_log: &bootstrap::Client<PortTransport>,
    ) -> Supervised {
        // Слоты принадлежат incarnation, и капы ушедшей ядро отзывает: потребители уходят
        // вместе с провайдером и поднимаются его сменой.
        let group = self.dependents(index);
        let dependents = u16::try_from(group.len()).unwrap_or(u16::MAX);
        let Some(component) = self.components.get_mut(usize::from(index)) else {
            return Supervised::Stopped(Stopped::Subsystem);
        };
        let exit_code = component
            .managed
            .running()
            .and_then(|running| running.process().exit_code().ok());
        let observed = component.managed.observe_exit(Timeout::POLL);
        let charged = component.charged_launches;

        let Ok(plan) = component.record.plan() else {
            return Supervised::Stopped(Stopped::Subsystem);
        };
        let launches = plan.launches();
        let on_exhausted = plan.on_exhausted();
        log_exit(kernel_log, plan.name(), exit_code);

        if observed.is_err() {
            let _ = kernel_log.log_str(LOG_TAG, "component budget not returned");
            return Supervised::Stopped(Stopped::Subsystem);
        }
        if dependents > 0 {
            log_takedown(kernel_log, plan.name(), dependents);
        }
        for &member in group.iter().rev() {
            self.stop(member);
        }
        if charged >= launches {
            let _ = kernel_log.log_str(LOG_TAG, "component restart limit reached");
            return give_up(on_exhausted, exit_code, dependents);
        }
        if let Err(failure) = self.launch(index) {
            if let Some(plan) = self
                .component(index)
                .and_then(|component| component.record.plan().ok())
            {
                log_restart_failure(kernel_log, plan.name(), failure);
            }
            return give_up(on_exhausted, exit_code, dependents);
        }
        self.charge_launch(index);
        for member in group {
            if self.launch(member).is_ok() {
                continue;
            }
            let _ = kernel_log.log_str(LOG_TAG, "dependent component not restarted");
            if self.halts(member) {
                return Supervised::Stopped(Stopped::Subsystem);
            }
        }
        Supervised::Restarted {
            exit_code,
            dependents,
        }
    }

    fn dependents(&self, index: u16) -> Vec<u16> {
        let mut taken = alloc::vec![false; self.components.len()];
        let Some(gone) = taken.get_mut(usize::from(index)) else {
            return Vec::new();
        };
        *gone = true;
        let mut group = Vec::new();
        // Порядок запуска топологический, и один проход по нему замыкает потребителей
        // транзитивно.
        for &candidate in &self.order {
            if candidate == index {
                continue;
            }
            let Some(component) = self.components.get(usize::from(candidate)) else {
                continue;
            };
            let depends = dependencies(component).is_ok_and(|mut edges| {
                edges.any(|edge| {
                    find_instance(&self.components, edge)
                        .is_some_and(|provider| taken[usize::from(provider)])
                })
            });
            if !depends {
                continue;
            }
            taken[usize::from(candidate)] = true;
            // Снятый компонент своей смены не получает: композиция держит его без запуска.
            if component.managed.running().is_some() {
                group.push(candidate);
            }
        }
        group
    }

    fn charge_launch(&mut self, index: u16) {
        if let Some(component) = self.components.get_mut(usize::from(index)) {
            component.charged_launches = component.charged_launches.saturating_add(1);
        }
    }

    fn halts(&self, index: u16) -> bool {
        self.components
            .get(usize::from(index))
            .and_then(|component| component.record.plan().ok())
            .is_none_or(|plan| matches!(plan.on_exhausted(), OnExhausted::Halt))
    }
}

/// Поднимает систему и держит её до завершения критического компонента.
pub fn supervise(kernel: Handle) -> Stopped {
    let kernel_log = bootstrap::Client::new(PortTransport::client(kernel));
    if relay::start(kernel).is_err() {
        // Отказ relay идёт в bootstrap-канал: по отказавшему пути диагностика не проходит.
        let _ = kernel_log.log_str(LOG_TAG, "log relay not started");
        return Stopped::Startup;
    }
    let stopped = run(kernel, &kernel_log);
    relay::shutdown(STOP_TIMEOUT);
    stopped
}

fn run(kernel: Handle, kernel_log: &bootstrap::Client<PortTransport>) -> Stopped {
    let Some(session) = relay::session(relay::SUPERVISOR_BADGE) else {
        let _ = kernel_log.log_str(LOG_TAG, "supervisor log session not minted");
        return Stopped::Startup;
    };
    let log = log::Client::new(PortTransport::client(&session));
    let _ = log.write_str(LOG_TAG, "started");

    let Some(relay_thread) = relay::thread_handle() else {
        let _ = log.write_str(LOG_TAG, "log relay thread unavailable");
        return Stopped::Startup;
    };

    let image = match SystemImage::acquire(kernel) {
        Ok(image) => image,
        Err(failure) => {
            let _ = log.write_str(LOG_TAG, failure.message());
            return failure.stopped();
        }
    };
    let plan = match image.plan(BOOT_PLAN_PATH) {
        Ok(plan) => plan,
        Err(failure) => {
            let _ = log.write_str(LOG_TAG, failure.message());
            return failure.stopped();
        }
    };

    run_composition(&image, plan, kernel, relay_thread, &log)
}

fn run_composition(
    image: &SystemImage,
    plan: BootPlan<'_>,
    kernel: Handle,
    relay_thread: Handle,
    log: &log::Client<PortTransport>,
) -> Stopped {
    match RootSystem::bring_up(image, plan, kernel, relay::delegated_session) {
        Ok(mut system) => {
            let _ = log.write_str(LOG_TAG, "composition started");
            let stopped = loop {
                match system.supervise_turn(relay_thread) {
                    Supervised::Restarted { .. } => {
                        let _ = log.write_str(LOG_TAG, "component restarted");
                    }
                    Supervised::Abandoned { .. } => {
                        let _ = log.write_str(LOG_TAG, "component abandoned");
                    }
                    Supervised::Extended => {}
                    Supervised::Stopped(stopped) => break stopped,
                }
            };
            system.shutdown();
            stopped
        }
        Err(failure) => {
            let _ = log.write_str(LOG_TAG, failure.message());
            failure.stopped()
        }
    }
}

const fn give_up(on_exhausted: OnExhausted, exit_code: Option<u32>, dependents: u16) -> Supervised {
    match on_exhausted {
        OnExhausted::Halt => Supervised::Stopped(Stopped::Subsystem),
        OnExhausted::Detach => Supervised::Abandoned {
            exit_code,
            dependents,
        },
    }
}

const fn refused_session(failure: BootFailure) -> ExtendError {
    match failure {
        BootFailure::Provider => ExtendError::NotFound,
        other => other.refused(),
    }
}

fn plan_needs_devices(plan: &BootPlan<'_>) -> bool {
    plan.components().any(|component| {
        component
            .slots()
            .any(|slot| matches!(slot, SlotSource::Irq(_) | SlotSource::Mmio(_)))
    })
}

fn session(
    components: &[Component<'_>],
    instance: &str,
    badge: NonZeroU64,
) -> Result<OwnedHandle, BootFailure> {
    let provider = Provider::of(components, instance)?;
    // Значки сессий провайдера с состоянием принадлежат ему одному: минтер супервизора на
    // его service-порт значков не выдаёт.
    if provider.plan.establishes_sessions() {
        return open_session(
            provider.roots.control(),
            provider.plan.startup_timeout(),
            WHOLE_EXPORT,
        );
    }
    provider.roots.session(badge).map_err(|_| BootFailure::Slot)
}

fn component_area(
    components: &[Component<'_>],
    instance: &str,
    scope: &str,
) -> Result<OwnedHandle, BootFailure> {
    let provider = Provider::of(components, instance)?;
    // Сузить сессию до области умеет лишь тот, кто её и заводит: сминченный значок области
    // не назначает.
    if !provider.plan.establishes_sessions() {
        return Err(BootFailure::Directory);
    }
    open_session(
        provider.roots.control(),
        provider.plan.startup_timeout(),
        scope,
    )
    .map_err(|_| BootFailure::Directory)
}

fn control_session(
    components: &[Component<'_>],
    instance: &str,
    badge: NonZeroU64,
) -> Result<OwnedHandle, BootFailure> {
    Provider::of(components, instance)?
        .roots
        .control_session(badge)
        .map_err(|_| BootFailure::Slot)
}

fn start_order(components: &[Component<'_>]) -> Result<Vec<u16>, BootFailure> {
    // Порядок запуска топологический: компонент поднимается после всех, кого
    // называют провайдеры его слотов и `after`.
    let count = components.len();
    let mut started = alloc::vec![false; count];
    let mut order = Vec::with_capacity(count);
    while order.len() < count {
        let mut progress = false;
        for index in 0..count {
            if started[index] {
                continue;
            }
            let mut ready = true;
            for edge in dependencies(&components[index])? {
                let edge = find_instance(components, edge).ok_or(BootFailure::Provider)?;
                ready &= started[usize::from(edge)];
            }
            if ready {
                started[index] = true;
                order.push(u16::try_from(index).map_err(|_| BootFailure::PlanOrder)?);
                progress = true;
            }
        }
        if !progress {
            return Err(BootFailure::PlanOrder);
        }
    }
    Ok(order)
}

fn dependencies<'a>(
    component: &'a Component<'_>,
) -> Result<impl Iterator<Item = &'a str>, BootFailure> {
    let plan = component.record.plan()?;
    Ok(plan
        .slots()
        .filter_map(|slot| match slot {
            SlotSource::Service { provider }
            | SlotSource::Directory { provider }
            | SlotSource::Control { provider } => Some(provider),
            _ => None,
        })
        .chain(plan.after()))
}

fn instance_name<'a>(component: &'a Component<'_>) -> &'a str {
    component.record.plan().map_or("", |plan| plan.name())
}

fn find_instance(components: &[Component<'_>], instance: &str) -> Option<u16> {
    components
        .iter()
        .position(|component| {
            component
                .record
                .plan()
                .is_ok_and(|plan| plan.name() == instance)
        })
        .and_then(|index| u16::try_from(index).ok())
}

fn open_session(
    control: BorrowedHandle<'_>,
    timeout: Timeout,
    scope: &str,
) -> Result<OwnedHandle, BootFailure> {
    let scope = Str::<SESSION_SCOPE_MAX>::new(scope).ok_or(BootFailure::Session)?;
    let session = component_sessions::Client::new(PortTransport::client(control))
        .with_wait(timeout)
        .open_session(scope)
        .map_err(|_| BootFailure::Session)?
        .map_err(|_| BootFailure::Session)?;
    Ok(OwnedHandle::adopt(session))
}

fn log_device_conflict(
    kernel_log: &bootstrap::Client<PortTransport>,
    component: &str,
    resource: Arguments<'_>,
) {
    let mut message = LogBuffer::<EXIT_MESSAGE_MAX>::new();
    if write!(
        message,
        "{component} takes {resource}, held by another component"
    )
    .is_ok()
    {
        let _ = kernel_log.log_str(LOG_TAG, message.as_str());
    }
}

fn log_restart_failure(
    kernel_log: &bootstrap::Client<PortTransport>,
    component: &str,
    failure: BootFailure,
) {
    let mut message = LogBuffer::<EXIT_MESSAGE_MAX>::new();
    if write!(message, "{component} not restarted: {}", failure.message()).is_ok() {
        let _ = kernel_log.log_str(LOG_TAG, message.as_str());
    }
}

fn log_takedown(kernel_log: &bootstrap::Client<PortTransport>, component: &str, dependents: u16) {
    let mut message = LogBuffer::<EXIT_MESSAGE_MAX>::new();
    let plural = if dependents == 1 { "" } else { "s" };
    if write!(
        message,
        "{component} takes down {dependents} dependent component{plural}"
    )
    .is_ok()
    {
        let _ = kernel_log.log_str(LOG_TAG, message.as_str());
    }
}

fn log_exit(
    kernel_log: &bootstrap::Client<PortTransport>,
    subsystem: &str,
    exit_code: Option<u32>,
) {
    let mut message = LogBuffer::<EXIT_MESSAGE_MAX>::new();
    let written = match exit_code {
        Some(code) => write!(message, "{subsystem} stopped with exit code {code}"),
        None => write!(message, "{subsystem} stopped; exit code unavailable"),
    };
    if written.is_ok() {
        let _ = kernel_log.log_str(LOG_TAG, message.as_str());
    }
}
