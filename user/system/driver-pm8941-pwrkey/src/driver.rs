//! Источник ввода `InputSource` одной кнопки PON: подключение по описанию платформы и передача
//! нажатий хабу по прерыванию кнопки.

use alloc::sync::Arc;
use core::fmt;

use component_startup::{
    AnyOf, Import, Providers, SessionEvent, SessionPort, StartupError,
    component_startup as startup_contract, serve_export, whole_export,
};
use input_abi::{
    AttachError, AxisList, Binding, Command, DeclareError, DeviceDescriptor, DeviceNumber, KeyList,
    UpdateError, Usage, input_inject, input_source,
};
use ipc::CallError;
use kernel_objects::{Delivery, ReceivedSession, Session, SessionTransport, Signal};
use platform_abi::{CONTROLLER_KEY, DescriptionError, MappedDescription, PlatformError, platform};
use runtime::{
    FAILED_EXIT_CODE, Instant, PortTransport, Timeout, exit, log_fmt, spawn_bound, spawn_owned,
};
use spmi_abi::{DeviceError, SpmiError, spmi, spmi_device};
use syscall::SIGNALED;

use crate::key::{DEBOUNCE_DEFAULT_US, Generation, Key, Transition, transition, usage};

const SUBTYPE: u16 = 0x05;
const RT_STS: u16 = 0x10;
const PULL_CTL: u16 = 0x70;
const DBC_CTL: u16 = 0x71;

const KEY_INTERRUPT: u16 = 0;
const KEY_INDEX: u16 = 0;

const CALL_TIMEOUT: Timeout = Timeout::from_secs(2);

/// Причина остановки программы кнопки.
#[derive(Debug)]
pub enum Stopped {
    /// Требование запуска не выдано.
    Startup(StartupError),
    /// Порт экспорта не создан.
    ExportPort,
    /// Поток экспорта не запущен.
    ExportThread,
    /// Порт экспорта закрыт.
    ExportPortClosed,
}

struct Source {
    key: &'static Key,
    spmi: Providers<spmi::Protocol>,
    platform: platform::Client<SessionTransport>,
    attached: bool,
    refused: bool,
}

struct Node {
    bus: spmi::Client<SessionTransport>,
    device: spmi_device::Client<SessionTransport>,
    base: u16,
    usage: Usage,
    debounce: u32,
    pull_up: bool,
}

struct Button {
    _bus: spmi::Client<SessionTransport>,
    device: spmi_device::Client<SessionTransport>,
    inject: input_inject::Client<SessionTransport>,
    number: DeviceNumber,
    signal: Signal,
    status: u16,
    status_bit: u8,
    window: u64,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Failure {
    Platform(CallError<PlatformError>),
    Mapping(kernel_objects::Error),
    Description(DescriptionError),
    NoNode,
    NoBase,
    NoController,
    NoBus,
    Startup(StartupError),
    UnknownCode(u32),
    Debounce(u32),
    Bus(CallError<SpmiError>),
    Device(CallError<DeviceError>),
    Descriptor,
    Declare(CallError<DeclareError>),
    Update(CallError<UpdateError>),
    Signal(kernel_objects::Error),
    Thread,
}

impl fmt::Display for Stopped {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Startup(error) => error.fmt(formatter),
            Self::ExportPort => formatter.write_str("export port refused"),
            Self::ExportThread => formatter.write_str("export thread refused"),
            Self::ExportPortClosed => formatter.write_str("export port closed"),
        }
    }
}

impl Source {
    fn bind(&self, inject: input_inject::Client<SessionTransport>) -> Result<(), Failure> {
        let node = Node::find(self.key, &self.platform, &self.spmi)?;
        let button = Button::bind(self.key, node, inject)?;

        spawn_owned(move || button.watch())
            .map(drop)
            .map_err(|_| Failure::Thread)
    }
}

impl input_source::Service<Delivery> for Source {
    fn attach(
        &mut self,
        _frame: &Delivery,
        inject: ReceivedSession<input_inject::Protocol>,
    ) -> Result<(), AttachError> {
        if self.attached {
            return Err(AttachError::Busy);
        }

        let inject = Session::from_session(inject)
            .client()
            .with_wait(CALL_TIMEOUT);

        match self.bind(inject) {
            Ok(()) => {
                log_fmt(format_args!("{} bound", self.key.compatible));
                self.attached = true;
                Ok(())
            }
            Err(failure) => {
                log_fmt(format_args!(
                    "{} not bound: {failure:?}",
                    self.key.compatible
                ));
                self.refused = true;
                Err(AttachError::Refused)
            }
        }
    }
}

impl Node {
    fn find(
        key: &Key,
        platform: &platform::Client<SessionTransport>,
        spmi: &Providers<spmi::Protocol>,
    ) -> Result<Self, Failure> {
        let description = platform.description().map_err(Failure::Platform)?;
        let mapped = MappedDescription::new(description)
            .map_err(|refused| Failure::Mapping(refused.error))?;
        let description = mapped.description().map_err(Failure::Description)?;

        let id = description
            .compatible_devices(key.compatible)
            .next()
            .map(|device| device.id().clone())
            .ok_or(Failure::NoNode)?;
        let node = description.node(&id).ok_or(Failure::NoNode)?;
        let compatible = spmi_abi::controller(&description, &id)
            .ok_or(Failure::NoController)?
            .strings("compatible")
            .map_err(Failure::Description)?
            .ok_or(Failure::NoController)?;
        let found = spmi
            .all(AnyOf::new(CONTROLLER_KEY, compatible))
            .map_err(Failure::Startup)?
            .next()
            .ok_or(Failure::NoBus)?
            .map_err(Failure::Startup)?;
        let bus = spmi
            .open(&found)
            .map_err(Failure::Startup)?
            .client()
            .with_wait(CALL_TIMEOUT);
        let device = Session::<spmi_device::Protocol>::from_session(
            bus.open(id.clone()).map_err(Failure::Bus)?,
        )
        .client()
        .with_wait(CALL_TIMEOUT);

        // База блока PON - первая ячейка `reg` узла, а без него - родителя, как в Linux.
        let parent = description.ancestors(&id).next();
        let reg = match node.cells("reg").map_err(Failure::Description)? {
            Some(reg) => Some(reg),
            None => parent
                .map(|parent| parent.node.cells("reg"))
                .transpose()
                .map_err(Failure::Description)?
                .flatten(),
        };
        let base = reg
            .and_then(|mut reg| reg.next())
            .and_then(|base| u16::try_from(base).ok())
            .ok_or(Failure::NoBase)?;

        let code = node.u32("linux,code").map_err(Failure::Description)?;
        let usage = usage(code).map_err(Failure::UnknownCode)?;

        let debounce = node
            .u32("debounce")
            .map_err(Failure::Description)?
            .unwrap_or(DEBOUNCE_DEFAULT_US);

        Ok(Self {
            bus,
            device,
            base,
            usage,
            debounce,
            pull_up: node.bytes("bias-pull-up").is_some(),
        })
    }
}

impl Button {
    fn bind(
        key: &Key,
        node: Node,
        inject: input_inject::Client<SessionTransport>,
    ) -> Result<Self, Failure> {
        let Node {
            bus,
            device,
            base,
            usage,
            debounce,
            pull_up,
        } = node;
        let register = |offset: u16| base.wrapping_add(offset);

        let subtype = device.read(register(SUBTYPE), 1)?;
        let generation = Generation::of(u8::try_from(subtype).unwrap_or_default());
        let mask = generation.mask();
        let field = generation
            .debounce_field(debounce)
            .ok_or(Failure::Debounce(debounce))?;

        device.update(register(DBC_CTL), mask, field)?;
        let pull_up = if pull_up { key.pull_up_bit } else { 0 };
        device.update(register(PULL_CTL), key.pull_up_bit, pull_up)?;

        // Поле общее для кнопок блока, и окно считается по записанному последним значению.
        let configured = device.read(register(DBC_CTL), 1)?;
        let window = generation.debounce_window(u8::try_from(configured).unwrap_or_default());

        let signal = Signal::from(device.irq(KEY_INTERRUPT)?);

        let mut keys = KeyList::new();
        keys.push(usage).map_err(|_| Failure::Descriptor)?;
        let axes = AxisList::new();
        let descriptor = DeviceDescriptor::new(key.name, Binding::None, &keys, &axes, 0, &axes)
            .ok_or(Failure::Descriptor)?;
        let number = inject.declare(descriptor).map_err(Failure::Declare)?;

        Ok(Self {
            _bus: bus,
            device,
            inject,
            number,
            signal,
            status: register(RT_STS),
            status_bit: key.status_bit,
            window,
        })
    }

    fn watch(self) -> u32 {
        let mut pressed = false;
        let mut quiet_until = 0;

        loop {
            if let Err(error) = self.signal.wait(SIGNALED, Timeout::INFINITE) {
                stop(&Failure::Signal(error));
            }

            if Instant::now().as_nanos() >= quiet_until {
                match self.report(pressed) {
                    Ok((set, time)) => {
                        pressed = set;
                        if !set {
                            quiet_until = time.saturating_add(self.window);
                        }
                    }
                    Err(failure) => stop(&failure),
                }
            }

            if let Err(error) = self.device.ack(KEY_INTERRUPT) {
                stop(&Failure::Device(error));
            }
        }
    }

    fn report(&self, pressed: bool) -> Result<(bool, u64), Failure> {
        let status = self.device.read(self.status, 1)?;
        let time = Instant::now().as_nanos();
        let set = status & u64::from(self.status_bit) != 0;

        let steps: &[bool] = match transition(pressed, set) {
            Transition::None => &[],
            Transition::Press => &[true],
            Transition::Release => &[false],
            Transition::PressAndRelease => &[true, false],
        };

        for down in steps {
            let mut command = Command::new();
            let built = if *down {
                command.press(KEY_INDEX)
            } else {
                command.release(KEY_INDEX)
            };
            built.map_err(|_| Failure::Descriptor)?;

            self.inject
                .update(self.number, Some(time), command.as_bytes())
                .map_err(Failure::Update)?;
        }

        Ok((set, time))
    }
}

impl From<CallError<DeviceError>> for Failure {
    fn from(error: CallError<DeviceError>) -> Self {
        Self::Device(error)
    }
}

/// Обслуживает экспорт `InputSource` кнопки `key` до закрытия порта экспорта. Отказ
/// подключения либо обслуживания кнопки завершает процесс.
pub fn run(
    key: &'static Key,
    spmi: Providers<spmi::Protocol>,
    platform: &Import<platform::Protocol>,
    parent: startup_contract::Client<PortTransport>,
) -> Result<(), Stopped> {
    let platform = platform
        .open()
        .map_err(Stopped::Startup)?
        .client()
        .with_wait(CALL_TIMEOUT);

    let export = Arc::new(SessionPort::create().map_err(|_| Stopped::ExportPort)?);
    let port = Arc::clone(&export);
    spawn_bound(move || serve_export::<input_source::Protocol>(parent, whole_export(&port)))
        .map_err(|_| Stopped::ExportThread)?;

    let mut source = Source {
        key,
        spmi,
        platform,
        attached: false,
        refused: false,
    };
    let mut events = export.events();

    while let Some(event) = events.recv() {
        if let SessionEvent::Frame(frame) = event {
            let _ = input_source::dispatch(&mut source, frame);
            if source.refused {
                exit(FAILED_EXIT_CODE)
            }
        }
    }

    Err(Stopped::ExportPortClosed)
}

fn stop(failure: &Failure) -> ! {
    log_fmt(format_args!("button stopped: {failure:?}"));
    exit(FAILED_EXIT_CODE)
}
