//! Композиция, которую rootkeeper поднимает при загрузке, и её supervisor.

use alloc::{format, vec::Vec};
use core::{
    fmt::{Arguments, Write as _},
    num::NonZeroU64,
};

use bootstrap_abi::bootstrap;
use device_resolver::{
    DeviceMmio, DeviceSelector, DeviceTreePlatform, PlatformDescription, ResolveError, acquire_mmio,
};
use device_resolver_aarch64::PLATFORM_BINDINGS;
use fdt::devicetree::DeviceTree;
use filesystem_abi::{PATH_MAX, namespace};
use ipc::wire::Str;
use log_abi::{LogBuffer, log};
use managed_component::{
    ComponentRoots, ComponentSlots, ManagedComponent, ManagedStartupError, launch_component,
};
use runtime::{
    IrqControl, MemoryRegion, OwnedHandle, PortTransport, ReadOnlyMapping, Resource, Timeout,
    wait_any,
};
use syscall::{Handle, Rights, SIGNALED, SyscallError, WaitItem};
use userland_image::{
    BOOT_PLAN_PATH, BootPlan, COMPONENT_PROGRAM_NAME, DecodedImage, DecodedProgram, OnExhausted,
    PlanComponent, PlanDecodeError, PlanDevice, SlotSource, decode, decode_plan,
};

use crate::{relay, sessions::SessionBroker};

const LOG_TAG: &str = "supervisor";

/// Конечный срок остановки компонента.
pub const STOP_TIMEOUT: Timeout = Timeout::from_secs(5);

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
    /// В образе нет программы компонента.
    ProgramMissing,
    /// Корневой metering resource недоступен.
    Resource,
    /// Полномочия провижининга устройств недоступны.
    DeviceAuthority,
    /// Описание платформы не дало ресурс, названный слотом.
    Device(ResolveError),
    /// Ресурс устройства уже отдан другому компоненту композиции.
    DeviceBusy,
    /// Провайдер сервисного слота не поднят.
    Provider,
    /// Broker сессий компонентов не поднят.
    Broker,
    /// Каталог компонента не выдан сессией файловой системы.
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
    /// incarnation.
    Restarted { exit_code: Option<u32> },
    /// Некритический компонент исчерпал запуски и снят с композиции, а сама
    /// композиция осталась поднятой.
    Abandoned { exit_code: Option<u32> },
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

/// Компонент композиции вместе с его записью плана.
pub struct Component<'a> {
    managed: ManagedComponent<ComponentRoots>,
    plan: PlanComponent<'a>,
    log: OwnedHandle,
}

/// Поднятая композиция rootkeeper.
pub struct RootSystem<'a> {
    components: Vec<Component<'a>>,
    order: Vec<u16>,
    devices: DeviceAuthority,
    kernel: Handle,
    image: &'a SystemImage,
    entries: DecodedImage<'a>,
    broker: SessionBroker,
    _resource: Resource,
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
            | Self::Resource
            | Self::Provider
            | Self::Broker
            | Self::Directory
            | Self::Slot
            | Self::Launch(_) => Stopped::Startup,
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
            Self::ProgramMissing => "component program missing",
            Self::Resource => "root resource unavailable",
            Self::DeviceAuthority => "platform description unavailable",
            Self::Device(_) => "device not described by the platform",
            Self::DeviceBusy => "device resource taken by another component",
            Self::Provider => "service provider not running",
            Self::Broker => "component session broker not started",
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

impl DeviceAuthority {
    const fn absent() -> Self {
        Self {
            description: None,
            _region: None,
            lines: None,
            windows: Vec::new(),
        }
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

impl Component<'_> {
    /// Managed-примитив компонента.
    #[must_use]
    pub const fn managed(&self) -> &ManagedComponent<ComponentRoots> {
        &self.managed
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
        let broker = SessionBroker::start().map_err(|_| BootFailure::Broker)?;
        let resource = Resource::self_resource().map_err(|_| BootFailure::Resource)?;
        let order = start_order(&plan).ok_or(BootFailure::PlanOrder)?;
        let devices = if plan_needs_devices(&plan) {
            DeviceAuthority::acquire(kernel)?
        } else {
            DeviceAuthority::absent()
        };

        let mut components = Vec::with_capacity(usize::from(plan.len()));
        for index in 0..plan.len() {
            let component = plan.component(index).ok_or(BootFailure::PlanOrder)?;
            let log = log_sessions(relay::component_badge(index)).ok_or(BootFailure::LogSession)?;
            components.push(Component {
                managed: ManagedComponent::new(&resource, component.memory_pages())
                    .map_err(BootFailure::Launch)?,
                plan: component,
                log,
            });
        }

        let mut system = Self {
            components,
            order,
            devices,
            kernel,
            image,
            entries,
            broker,
            _resource: resource,
        };
        for index in system.order.clone() {
            if let Err(failure) = system.launch(index) {
                // Поднятые компоненты уходят вместе с загрузкой: иначе они
                // держат линии и окна устройств за композицией, которой нет.
                system.shutdown();
                return Err(failure);
            }
        }
        Ok(system)
    }

    /// Компонент с номером `index` в плане.
    #[must_use]
    pub fn component(&self, index: u16) -> Option<&Component<'a>> {
        self.components.get(usize::from(index))
    }

    /// Номера компонентов в порядке запуска. Остановка идёт в обратном.
    #[must_use]
    pub fn start_order(&self) -> &[u16] {
        &self.order
    }

    /// Оборот supervisor: ждёт завершения любого компонента либо потока
    /// `relay_thread`, записывает исход в журнал ядра и поднимает ушедшему
    /// компоненту смену.
    ///
    /// Завершение потока пересылки критично: восстановить его композиция не
    /// умеет.
    pub fn supervise_turn(&mut self, relay_thread: Handle) -> Supervised {
        let kernel_log = bootstrap::Client::new(PortTransport::client(self.kernel));
        let mut waits = Vec::with_capacity(self.components.len() + 1);
        let mut watched: Vec<u16> = Vec::with_capacity(self.components.len());
        for index in 0..self.plan_len() {
            let Some(running) = self.component(index).and_then(|c| c.managed.running()) else {
                continue;
            };
            waits.push(WaitItem::new(running.process().handle().as_raw(), SIGNALED));
            watched.push(index);
        }
        waits.push(WaitItem::new(relay_thread, SIGNALED));

        match wait_any(&waits, Timeout::INFINITE) {
            Ok(index) if index < watched.len() => {
                self.replace_component(watched[index], &kernel_log)
            }
            Ok(_) => {
                log_exit(&kernel_log, "log relay", relay::exit_code());
                Supervised::Stopped(Stopped::Subsystem)
            }
            Err(_) => {
                let _ = kernel_log.log_str(LOG_TAG, "supervisor wait failed");
                Supervised::Stopped(Stopped::Subsystem)
            }
        }
    }

    /// Завершает компоненты композиции в порядке, обратном порядку запуска.
    pub fn shutdown(&mut self) {
        for index in self.order.clone().into_iter().rev() {
            if let Some(component) = self.components.get_mut(usize::from(index)) {
                let _ = component.managed.stop(SHUTDOWN_EXIT_CODE, STOP_TIMEOUT);
            }
        }
        self.broker.shutdown();
    }

    fn launch(&mut self, index: u16) -> Result<(), BootFailure> {
        let plan = self.component(index).ok_or(BootFailure::PlanOrder)?.plan;
        let program = component_program(&self.entries, plan)?;
        let slots = self.provision(index, plan)?;
        {
            let component = self
                .components
                .get_mut(usize::from(index))
                .ok_or(BootFailure::PlanOrder)?;
            launch_component(
                &mut component.managed,
                &program,
                plan.name(),
                &component.log,
                plan.startup_timeout(),
                slots,
            )
            .map_err(BootFailure::Launch)?;
        }
        let control = self
            .component(index)
            .and_then(|component| component.managed.running())
            .ok_or(BootFailure::Provider)?
            .roots()
            .control();
        self.broker
            .publish(index, plan.executable(), control, plan.startup_timeout())
            .map_err(|_| BootFailure::Broker)?;
        Ok(())
    }

    fn provision(
        &mut self,
        index: u16,
        plan: PlanComponent<'a>,
    ) -> Result<ComponentSlots, BootFailure> {
        let badge = relay::component_badge(index);
        let kernel_log = bootstrap::Client::new(PortTransport::client(self.kernel));
        let mut slots = ComponentSlots::default();
        for source in plan.slots() {
            let capability = match source {
                SlotSource::Userland => self
                    .image
                    .region()
                    .handle()
                    .duplicate(Rights::READ | Rights::TRANSFER | Rights::DUPLICATE, None)
                    .map_err(|_| BootFailure::Slot)?,
                SlotSource::Irq(device) => self.devices.line(device, plan.name(), &kernel_log)?,
                SlotSource::Mmio(device) => {
                    self.devices
                        .window(self.kernel, device, index, plan.name(), &kernel_log)?
                }
                SlotSource::Service { provider } => self.session(provider, badge)?,
                SlotSource::Directory { provider } => {
                    component_area(&self.session(provider, badge)?, plan)?
                }
                SlotSource::Supervisor => self.broker.session().ok_or(BootFailure::Broker)?,
            };
            slots.push(capability).map_err(BootFailure::Launch)?;
        }
        Ok(slots)
    }

    fn session(&self, provider: u16, badge: NonZeroU64) -> Result<OwnedHandle, BootFailure> {
        self.component(provider)
            .and_then(|component| component.managed.running())
            .ok_or(BootFailure::Provider)?
            .roots()
            .session(badge)
            .map_err(|_| BootFailure::Slot)
    }

    fn plan_len(&self) -> u16 {
        u16::try_from(self.components.len()).unwrap_or(u16::MAX)
    }

    fn replace_component(
        &mut self,
        index: u16,
        kernel_log: &bootstrap::Client<PortTransport>,
    ) -> Supervised {
        let Some(component) = self.components.get_mut(usize::from(index)) else {
            return Supervised::Stopped(Stopped::Subsystem);
        };
        let plan = component.plan;
        let exit_code = component
            .managed
            .running()
            .and_then(|running| running.process().exit_code().ok());
        let observed = component.managed.observe_exit(Timeout::POLL);
        let generation = component.managed.generation();
        log_exit(kernel_log, plan.name(), exit_code);
        if observed.is_err() {
            let _ = kernel_log.log_str(LOG_TAG, "component budget not returned");
            return Supervised::Stopped(Stopped::Subsystem);
        }
        if generation >= plan.launches() {
            let _ = kernel_log.log_str(LOG_TAG, "component restart limit reached");
            return give_up(plan, exit_code);
        }
        if self.launch(index).is_err() {
            let _ = kernel_log.log_str(LOG_TAG, "component not restarted");
            return give_up(plan, exit_code);
        }
        Supervised::Restarted { exit_code }
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
    relay::install_names(plan.components().map(|component| component.name()));

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

const fn give_up(plan: PlanComponent<'_>, exit_code: Option<u32>) -> Supervised {
    match plan.on_exhausted() {
        OnExhausted::Halt => Supervised::Stopped(Stopped::Subsystem),
        OnExhausted::Detach => Supervised::Abandoned { exit_code },
    }
}

fn plan_needs_devices(plan: &BootPlan<'_>) -> bool {
    plan.components().any(|component| {
        component
            .slots()
            .any(|slot| matches!(slot, SlotSource::Irq(_) | SlotSource::Mmio(_)))
    })
}

fn start_order(plan: &BootPlan<'_>) -> Option<Vec<u16>> {
    // Порядок запуска топологический: компонент поднимается после всех, кого
    // называют его requires и after.
    let count = usize::from(plan.len());
    let mut started = alloc::vec![false; count];
    let mut order = Vec::with_capacity(count);
    while order.len() < count {
        let mut progress = false;
        for index in 0..count {
            if started[index] {
                continue;
            }
            let component = plan.component(u16::try_from(index).ok()?)?;
            let ready = component
                .requires()
                .chain(component.after())
                .all(|edge| started.get(usize::from(edge)).copied().unwrap_or(false));
            if ready {
                started[index] = true;
                order.push(u16::try_from(index).ok()?);
                progress = true;
            }
        }
        if !progress {
            return None;
        }
    }
    Some(order)
}

fn component_program<'a>(
    entries: &DecodedImage<'a>,
    plan: PlanComponent<'a>,
) -> Result<DecodedProgram<'a>, BootFailure> {
    let record = format!("{}/{COMPONENT_PROGRAM_NAME}", plan.executable());
    entries
        .entry(&record)
        .and_then(|entry| entry.program().ok())
        .ok_or(BootFailure::ProgramMissing)
}

fn component_area(
    namespace_session: &OwnedHandle,
    plan: PlanComponent<'_>,
) -> Result<OwnedHandle, BootFailure> {
    let directory = Str::<PATH_MAX>::new(plan.executable()).ok_or(BootFailure::Directory)?;
    let session = namespace::Client::new(PortTransport::client(namespace_session))
        .with_wait(plan.startup_timeout())
        .resolve(directory)
        .map_err(|_| BootFailure::Directory)?
        .map_err(|_| BootFailure::Directory)?;
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
