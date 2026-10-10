//! Источник ввода `InputSource` одной кнопки PON: подключение по описанию платформы и передача
//! нажатий хабу по прерыванию кнопки.

use alloc::boxed::Box;
use core::fmt;

use component_startup::{
    Import, StartupError, component_startup as startup_contract, serve_export, whole_export,
};
use input_abi::{
    AttachError, AxisList, Binding, Command, DeclareError, DeviceDescriptor, KeyList, UpdateError,
    Usage, input_inject, input_source,
};
use ipc::{CallError, Listener, RecvError, wire::MESSAGE_INLINE_MAX};
use kernel_objects::{
    Delivery, IssuedHandle, MemoryRegion, Port, ReadOnlyMapping, Session, SessionTransport, Signal,
    kind,
};
use platform_abi::{Description, DescriptionError, PlatformError, platform};
use runtime::{FAILED_EXIT_CODE, Instant, PortTransport, Timeout, exit, log_fmt, spawn_owned};
use spmi_abi::{DeviceError, SpmiError, spmi, spmi_device};
use syscall::SIGNALED;

use crate::key::{DEBOUNCE_DEFAULT_US, Generation, Key, Transition, transition, usage};

const SUBTYPE: u16 = 0x05;
const RT_STS: u16 = 0x10;
const PULL_CTL: u16 = 0x70;
const DBC_CTL: u16 = 0x71;

const KEY_INTERRUPT: u16 = 0;
const KEY_INDEX: u16 = 0;

const CELL_BYTES: usize = 4;
const SEPARATOR: char = '/';

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
    bus: spmi::Client<SessionTransport>,
    platform: platform::Client<SessionTransport>,
    attached: bool,
    refused: bool,
}

struct Node {
    base: u16,
    usage: Usage,
    debounce: u32,
    pull_up: bool,
}

struct Button {
    device: spmi_device::Client<SessionTransport>,
    inject: input_inject::Client<SessionTransport>,
    number: u16,
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
        let node = Node::find(self.key, &self.platform)?;
        let button = Button::bind(self.key, &self.bus, &node, inject)?;

        spawn_owned(move || button.watch())
            .map(drop)
            .map_err(|_| Failure::Thread)
    }
}

impl input_source::Service<Delivery> for Source {
    fn attach(
        &mut self,
        _frame: &Delivery,
        inject: ipc::wire::Session<input_inject::Protocol, IssuedHandle<kind::Port>>,
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
    fn find(key: &Key, platform: &platform::Client<SessionTransport>) -> Result<Self, Failure> {
        let description = platform.description().map_err(Failure::Platform)?;
        let mapping = ReadOnlyMapping::new(MemoryRegion::from(description.region))
            .map_err(|refused| Failure::Mapping(refused.error))?;
        let description = Description::new(mapping.as_bytes(), description.offset)
            .map_err(Failure::Description)?;

        // Шина выдаёт программе первый свободный узел совместимости, и драйвер читает первый
        // доступный узел той же совместимости.
        let id = description
            .compatible_devices(key.compatible)
            .next()
            .map(|device| device.id().clone())
            .ok_or(Failure::NoNode)?;
        let node = description.node(&id).ok_or(Failure::NoNode)?;

        // База блока PON - первая ячейка `reg` узла, а без него - родителя, как в Linux.
        let parent = id
            .as_str()
            .rsplit_once(SEPARATOR)
            .and_then(|(parent, _)| description.find(parent));
        let base = node
            .bytes("reg")
            .or_else(|| parent.and_then(|parent| parent.bytes("reg")))
            .and_then(|reg| reg.first_chunk::<CELL_BYTES>())
            .map(|cell| u32::from_be_bytes(*cell))
            .and_then(|base| u16::try_from(base).ok())
            .ok_or(Failure::NoBase)?;

        let code = node.u32("linux,code").map_err(Failure::Description)?;
        let usage = usage(code).map_err(Failure::UnknownCode)?;

        let debounce = node
            .u32("debounce")
            .map_err(Failure::Description)?
            .unwrap_or(DEBOUNCE_DEFAULT_US);

        Ok(Self {
            base,
            usage,
            debounce,
            pull_up: node.bytes("bias-pull-up").is_some(),
        })
    }

    fn register(&self, offset: u16) -> u16 {
        self.base.wrapping_add(offset)
    }
}

impl Button {
    fn bind(
        key: &Key,
        bus: &spmi::Client<SessionTransport>,
        node: &Node,
        inject: input_inject::Client<SessionTransport>,
    ) -> Result<Self, Failure> {
        let compatible = key.compatible;
        let device = Session::<spmi_device::Protocol>::from_session(
            bus.open(compatible).map_err(Failure::Bus)?,
        )
        .client()
        .with_wait(CALL_TIMEOUT);

        let subtype = device.read(node.register(SUBTYPE), 1)?;
        let generation = Generation::of(u8::try_from(subtype).unwrap_or_default());
        let mask = generation.mask();
        let field = generation
            .debounce_field(node.debounce)
            .ok_or(Failure::Debounce(node.debounce))?;

        device.update(node.register(DBC_CTL), mask, field)?;
        let pull_up = if node.pull_up { key.pull_up_bit } else { 0 };
        device.update(node.register(PULL_CTL), key.pull_up_bit, pull_up)?;

        // Поле общее для кнопок блока, и окно считается по записанному последним значению.
        let configured = device.read(node.register(DBC_CTL), 1)?;
        let window = generation.debounce_window(u8::try_from(configured).unwrap_or_default());

        let signal = Signal::from(device.irq(KEY_INTERRUPT)?);

        let mut keys = KeyList::new();
        keys.push(node.usage).map_err(|_| Failure::Descriptor)?;
        let axes = AxisList::new();
        let descriptor = DeviceDescriptor::new(key.name, Binding::None, &keys, &axes, 0, &axes)
            .ok_or(Failure::Descriptor)?;
        let number = inject.declare(descriptor).map_err(Failure::Declare)?;

        Ok(Self {
            device,
            inject,
            number,
            signal,
            status: node.register(RT_STS),
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
    spmi: &Import<spmi::Protocol>,
    platform: &Import<platform::Protocol>,
    parent: startup_contract::Client<PortTransport>,
) -> Result<(), Stopped> {
    let bus = spmi
        .open()
        .map_err(Stopped::Startup)?
        .client()
        .with_wait(CALL_TIMEOUT);
    let platform = platform
        .open()
        .map_err(Stopped::Startup)?
        .client()
        .with_wait(CALL_TIMEOUT);

    let export: &'static Port =
        Box::leak(Box::new(Port::create().map_err(|_| Stopped::ExportPort)?));

    // Поток экспорта работает до конца процесса.
    spawn_owned(move || {
        let failure = serve_export::<input_source::Protocol>(&parent, whole_export(export));
        log_fmt(format_args!("export stopped: {failure:?}"));
        exit(FAILED_EXIT_CODE)
    })
    .map_err(|_| Stopped::ExportThread)?;

    let mut source = Source {
        key,
        bus,
        platform,
        attached: false,
        refused: false,
    };
    let transport = PortTransport::new(export.borrow());
    let mut bytes = [0u8; MESSAGE_INLINE_MAX];

    loop {
        match transport.recv(&mut bytes, Timeout::INFINITE) {
            Ok(frame) => {
                let _ = input_source::dispatch(&mut source, frame);
                // Процесс завершается после ответа `Refused`.
                if source.refused {
                    exit(FAILED_EXIT_CODE)
                }
            }
            Err(RecvError::SenderGone(_) | RecvError::Discarded | RecvError::Timeout) => {}
            Err(RecvError::Closed) => return Err(Stopped::ExportPortClosed),
        }
    }
}

fn stop(failure: &Failure) -> ! {
    log_fmt(format_args!("button stopped: {failure:?}"));
    exit(FAILED_EXIT_CODE)
}
