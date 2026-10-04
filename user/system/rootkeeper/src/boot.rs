use alloc::format;
use core::num::NonZeroU64;

use bootstrap_abi::bootstrap;
use component_abi::{PROTOCOL_ID_MAX, decode_manifest};
use kernel_objects::{MemoryRegion, Session, kind};
use log_abi::log;
use log_relay::{KernelLogSink, LogRelayThread};
use runtime::{Mutex, PortTransport, Timeout};
use syscall::{Handle, ProcessName};
use userland::ProgramView;
use userland_image::{
    BOOT_PLAN_PATH, BootPlan, COMPONENT_PROGRAM_NAME, DecodedImage, PlanDecodeError, decode,
    decode_plan,
};

use crate::{
    AddFailure, BudgetFailure, Component, RootSystem, Stopped, SystemImage,
    relay::{ComponentNames, log_badge},
    system::log_line,
    turn,
};

/// Типы отказов при получении userland-образа и подготовке компонентов из плана загрузки.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum BootFailure {
    /// Userland-образ недоступен либо не разобран.
    BadImage,
    /// В образе нет записи плана загрузочной композиции.
    PlanMissing,
    /// Запись плана не разобрана.
    Plan(PlanDecodeError),
    /// В образе отсутствует программа, указанная в плане загрузки.
    ProgramMissing,
    /// Ресурса композиции не хватило.
    Budget(BudgetFailure),
}

impl BootFailure {
    /// Код выхода rootkeeper, соответствующий отказу.
    #[must_use]
    pub const fn stopped(self) -> Stopped {
        match self {
            Self::BadImage | Self::PlanMissing | Self::Plan(_) | Self::ProgramMissing => {
                Stopped::BadImage
            }
            Self::Budget(_) => Stopped::Startup,
        }
    }

    pub(crate) const fn message(self) -> &'static str {
        match self {
            Self::BadImage => "userland image unavailable",
            Self::PlanMissing => "boot plan record missing",
            Self::Plan(_) => "boot plan not decoded",
            Self::ProgramMissing => "component program missing",
            Self::Budget(BudgetFailure::LogSession) => "log session not minted",
            Self::Budget(BudgetFailure::Root) => "root resource unavailable",
            Self::Budget(BudgetFailure::Reserve) => "component reserve not granted",
            Self::Budget(BudgetFailure::StartupPort) => "startup port not created",
            Self::Budget(BudgetFailure::Hardware) => "hardware session not granted",
        }
    }
}

impl SystemImage {
    /// Забирает у ядра capability образа и отображает его read-only.
    pub fn acquire(kernel: Handle<kind::Port>) -> Result<Self, BootFailure> {
        let client = bootstrap::Client::new(PortTransport::new(kernel));
        let region = client
            .acquire_userland_image()
            .map_err(|_| BootFailure::BadImage)?
            .map(MemoryRegion::from)
            .map_err(|_| BootFailure::BadImage)?;
        let mapping = region
            .map_full_read_only()
            .map_err(|_| BootFailure::BadImage)?;
        Ok(Self { region, mapping })
    }

    /// Таблица записей образа.
    pub fn entries(&self) -> Result<DecodedImage<'_>, BootFailure> {
        decode(self.mapping.as_bytes()).map_err(|_| BootFailure::BadImage)
    }

    /// Разбирает план загрузочной композиции из записи `entry`.
    pub fn plan(&self, entry: &str) -> Result<BootPlan<'_>, BootFailure> {
        let record = self
            .entries()?
            .entry(entry)
            .ok_or(BootFailure::PlanMissing)?;
        decode_plan(record.data()).map_err(BootFailure::Plan)
    }
}

impl<'a> RootSystem<'a> {
    /// Добавляет начальные компоненты через API родителя. План нужен только этому адаптеру.
    pub fn bring_up(
        image: &'a SystemImage,
        plan: BootPlan<'a>,
        kernel: Handle<kind::Port>,
        log_sessions: impl Fn(NonZeroU64) -> Option<Session<log::Protocol>>,
    ) -> Result<Self, BootFailure> {
        let boot = plan.component(0).ok_or(BootFailure::PlanMissing)?;
        let entries = image.entries()?;
        let path = format!("{}/{COMPONENT_PROGRAM_NAME}", boot.executable());
        let program = entries
            .entry(&path)
            .and_then(|entry| userland::decode_program(entry.data()).ok())
            .ok_or(BootFailure::ProgramMissing)?;
        let manifest = decode_manifest(program.manifest()).map_err(|_| BootFailure::BadImage)?;
        if manifest.requirements().next().is_some()
            || manifest
                .exports()
                .any(|export| export.protocol().len() > PROTOCOL_ID_MAX)
        {
            return Err(BootFailure::BadImage);
        }
        let mut system =
            Self::new(image, kernel, boot.name(), manifest).map_err(BootFailure::Budget)?;

        for (index, component) in plan.components().enumerate().skip(1) {
            let record = u16::try_from(index).map_err(|_| BootFailure::ProgramMissing)?;
            let path = format!("{}/{COMPONENT_PROGRAM_NAME}", component.executable());
            let program = entries
                .entry(&path)
                .and_then(|entry| userland::decode_program(entry.data()).ok())
                .ok_or(BootFailure::ProgramMissing)?;
            let log = log_sessions(log_badge(record))
                .ok_or(BootFailure::Budget(BudgetFailure::LogSession))?;

            system
                .add(
                    Component {
                        name: ProcessName::new(component.name()).ok_or(BootFailure::BadImage)?,
                        program,
                        memory_limit: component.memory_limit(),
                        restarts: component.restarts(),
                    },
                    log,
                )
                .map_err(|failure| match failure {
                    AddFailure::Resource(_) => BootFailure::Budget(BudgetFailure::Reserve),
                    AddFailure::Name | AddFailure::Capacity | AddFailure::Declaration => {
                        BootFailure::ProgramMissing
                    }
                })?;
        }
        crate::mount::start(&mut system, plan, kernel)?;
        Ok(system)
    }
}

/// Поднимает композицию загрузочного плана и служит её родителем. Возвращается только при
/// отказе подъёма либо выходе из оборота.
pub fn run(kernel: Handle<kind::Port>) -> Stopped {
    let image = match SystemImage::acquire(kernel) {
        Ok(image) => image,
        Err(failure) => return refuse_boot(kernel, failure),
    };
    let plan = match image.plan(BOOT_PLAN_PATH) {
        Ok(plan) => plan,
        Err(failure) => return refuse_boot(kernel, failure),
    };
    let Ok(relay) = LogRelayThread::start(ComponentNames::of(&plan), KernelLogSink::new(kernel))
    else {
        log_line(kernel, format_args!("log relay not started"));
        return Stopped::Startup;
    };
    let system = match RootSystem::bring_up(&image, plan, kernel, |badge| relay.session(badge).ok())
    {
        Ok(system) => Mutex::new(system),
        Err(failure) => return refuse_boot(kernel, failure),
    };
    log_line(kernel, format_args!("composition started"));

    while turn(&system, Timeout::INFINITE).is_continue() {}
    log_line(kernel, format_args!("startup port closed"));
    Stopped::Subsystem
}

fn refuse_boot(kernel: Handle<kind::Port>, failure: BootFailure) -> Stopped {
    log_line(kernel, format_args!("{}", failure.message()));
    failure.stopped()
}
