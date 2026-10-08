//! Привязка GPU через слот шины.

use display_abi::{Area, DisplayDimension, DisplayError, SURFACE_PIXEL_BYTES};
use ipc::wire::IpcError;
use kernel_objects::{IrqLine, Mapping, SessionTransport, Signal};
use runtime::{Timeout, log_fmt};
use syscall::SIGNALED;
use virtio::{
    Buffer, Completion, DeviceMemory, DeviceType, QUEUE_REGION_BYTES, QueueError, SplitQueue,
    Transport, TransportError,
    gpu::{Command, DisplayInfo, Format, GpuError, Rect},
};
use virtio_mmio_abi::{SlotError, VirtioMmioError, virtio_mmio, virtio_slot};

use crate::{
    memory::DmaRegion,
    registers::{RemoteRegisters, SlotFailure},
};

const SLOT_CALL_TIMEOUT: Timeout = Timeout::from_secs(2);
const COMPLETION_TIMEOUT: Timeout = Timeout::from_secs(2);
const BUSY_ATTEMPTS: usize = 10;
const BUSY_PAUSE: Timeout = Timeout::from_millis(5);
const CONTROL_QUEUE: u16 = 0;
const CONTROL_QUEUE_SIZE: u16 = 2;
const RESOURCE_ID: u32 = 1;
const RESPONSE_OFFSET: usize = 512;
const RESPONSE_BYTES: usize = 512;
const COMMANDS_REGION_BYTES: u64 = (RESPONSE_OFFSET + RESPONSE_BYTES) as u64;
const PIXEL_BYTES: u64 = SURFACE_PIXEL_BYTES as u64;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum BindRefusal {
    Bus(VirtioMmioError),
    BusCall(IpcError),
    Line(SlotError),
    Transport(TransportError<SlotFailure>),
    Command(CommandFailure),
    Memory(kernel_objects::Error),
    NoScanout,
    Geometry(u32, u32),
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum CommandFailure {
    Slot(SlotFailure),
    Queue(QueueError<SlotFailure>),
    Gpu(GpuError),
    Line(kernel_objects::Error),
    Memory,
    NotReady,
    UnexpectedCompletion,
}

pub(crate) struct Gpu {
    device: Device,
    dimension: DisplayDimension,
    scanout: u32,
}

struct Device {
    transport: Transport<RemoteRegisters>,
    line: IrqLine,
    queue: Option<SplitQueue>,
    queue_memory: DmaRegion,
    commands: DmaRegion,
    framebuffer: Option<DmaRegion>,
}

impl From<SlotFailure> for CommandFailure {
    fn from(failure: SlotFailure) -> Self {
        Self::Slot(failure)
    }
}

impl From<QueueError<SlotFailure>> for CommandFailure {
    fn from(error: QueueError<SlotFailure>) -> Self {
        Self::Queue(error)
    }
}

impl From<GpuError> for CommandFailure {
    fn from(error: GpuError) -> Self {
        Self::Gpu(error)
    }
}

impl From<virtio::OutOfRange> for CommandFailure {
    fn from(_: virtio::OutOfRange) -> Self {
        Self::Memory
    }
}

impl From<CommandFailure> for BindRefusal {
    fn from(failure: CommandFailure) -> Self {
        Self::Command(failure)
    }
}

impl From<kernel_objects::Error> for BindRefusal {
    fn from(error: kernel_objects::Error) -> Self {
        Self::Memory(error)
    }
}

impl Gpu {
    pub(crate) fn bind(bus: &virtio_mmio::Client<SessionTransport>) -> Result<Self, BindRefusal> {
        let mut device = Device::open(bus)?;
        match device.attach() {
            Ok((scanout, dimension)) => Ok(Self {
                device,
                dimension,
                scanout,
            }),
            Err(refusal) => {
                device.release();
                Err(refusal)
            }
        }
    }

    pub(crate) fn dimension(&self) -> DisplayDimension {
        self.dimension
    }

    pub(crate) fn present(&mut self, surface: &Mapping, area: Area) -> Result<(), DisplayError> {
        self.dimension.check_area(area)?;
        if self.device.queue.is_none() {
            self.device
                .initialize()
                .and_then(|()| self.device.attach_resource(self.scanout, self.dimension))
                .and_then(|()| {
                    self.device
                        .present(
                            Rect {
                                x: 0,
                                y: 0,
                                width: self.dimension.width,
                                height: self.dimension.height,
                            },
                            0,
                        )
                        .map_err(BindRefusal::from)
                })
                .map_err(|refusal| {
                    log_fmt(format_args!("gpu recovery refused: {refusal:?}"));
                    DisplayError::Hardware
                })?;
        }
        let stride = self.dimension.surface_stride();
        let first = start_offset(area, stride).ok_or(DisplayError::OutOfBounds)?;
        let first_byte = usize::try_from(first).map_err(|_| DisplayError::OutOfBounds)?;
        let row = usize::try_from(u64::from(area.width) * PIXEL_BYTES)
            .map_err(|_| DisplayError::OutOfBounds)?;
        let stride = usize::try_from(stride).map_err(|_| DisplayError::OutOfBounds)?;
        let height = usize::try_from(area.height).map_err(|_| DisplayError::OutOfBounds)?;
        let framebuffer = self
            .device
            .framebuffer
            .as_mut()
            .ok_or(DisplayError::Hardware)?;
        for line in 0..height {
            framebuffer
                .copy_from(surface, first_byte + line * stride, row)
                .map_err(|_| DisplayError::OutOfBounds)?;
        }
        let rect = Rect {
            x: area.x,
            y: area.y,
            width: area.width,
            height: area.height,
        };
        self.device.present(rect, first).map_err(|failure| {
            log_fmt(format_args!("present refused by the device: {failure:?}"));
            DisplayError::Hardware
        })
    }

    pub(crate) fn release(self) {
        self.device.release();
    }
}

impl Device {
    fn open(bus: &virtio_mmio::Client<SessionTransport>) -> Result<Self, BindRefusal> {
        let slot = open_slot(bus)?.client().with_wait(SLOT_CALL_TIMEOUT);
        let line = slot
            .irq()
            .map_err(|error| error.fold(BindRefusal::BusCall, BindRefusal::Line))?;
        let transport = Transport::probe(RemoteRegisters::new(slot), DeviceType::Gpu)
            .map_err(BindRefusal::Transport)?;
        let queue_memory = DmaRegion::new(QUEUE_REGION_BYTES as u64)?;
        let commands = DmaRegion::new(COMMANDS_REGION_BYTES)?;
        let mut device = Self {
            transport,
            line: IrqLine::from(line),
            queue: None,
            queue_memory,
            commands,
            framebuffer: None,
        };
        if let Err(refusal) = device.initialize() {
            device.release();
            return Err(refusal);
        }
        Ok(device)
    }

    fn initialize(&mut self) -> Result<(), BindRefusal> {
        self.queue = None;
        self.transport
            .negotiate(0)
            .map_err(BindRefusal::Transport)?;
        let queue = SplitQueue::new(
            &mut self.transport,
            &mut self.queue_memory,
            CONTROL_QUEUE,
            CONTROL_QUEUE_SIZE,
        )
        .map_err(CommandFailure::from)?;
        self.transport.start().map_err(CommandFailure::from)?;
        self.queue = Some(queue);
        Ok(())
    }

    fn attach(&mut self) -> Result<(u32, DisplayDimension), BindRefusal> {
        let (scanout, rect) = self.first_scanout()?;
        let dimension = DisplayDimension {
            width: rect.width,
            height: rect.height,
        };
        let geometry = BindRefusal::Geometry(rect.width, rect.height);
        let size = dimension
            .surface_size()
            .filter(|size| *size != 0)
            .ok_or(geometry)?;
        u32::try_from(size).map_err(|_| geometry)?;
        self.framebuffer = Some(DmaRegion::new(size)?);
        self.attach_resource(scanout, dimension)?;
        Ok((scanout, dimension))
    }

    fn attach_resource(
        &mut self,
        scanout: u32,
        dimension: DisplayDimension,
    ) -> Result<(), BindRefusal> {
        let geometry = BindRefusal::Geometry(dimension.width, dimension.height);
        let length = dimension
            .surface_size()
            .and_then(|size| u32::try_from(size).ok())
            .ok_or(geometry)?;
        let framebuffer = self.framebuffer.as_ref().ok_or(CommandFailure::NotReady)?;
        let backing = [Buffer {
            address: framebuffer.physical_address(),
            length,
        }];
        self.execute(Command::ResourceCreate2d {
            resource_id: RESOURCE_ID,
            format: Format::B8G8R8X8Unorm,
            width: dimension.width,
            height: dimension.height,
        })?;
        self.execute(Command::ResourceAttachBacking {
            resource_id: RESOURCE_ID,
            backing: &backing,
        })?;
        self.execute(Command::SetScanout {
            scanout_id: scanout,
            resource_id: RESOURCE_ID,
            rect: Rect {
                x: 0,
                y: 0,
                width: dimension.width,
                height: dimension.height,
            },
        })?;
        Ok(())
    }

    fn release(mut self) {
        if let Err(refusal) = self.transport.reset() {
            // Устройство может обращаться к DMA-регионам до подтверждённого сброса.
            log_fmt(format_args!("reset failed: {refusal:?}"));
            core::mem::forget((self.queue_memory, self.commands, self.framebuffer));
        }
    }

    fn first_scanout(&mut self) -> Result<(u32, Rect), BindRefusal> {
        let response = self.execute(Command::GetDisplayInfo)?;
        let info = DisplayInfo::parse(&response).map_err(CommandFailure::Gpu)?;
        (0u32..)
            .zip(info.scanouts)
            .find(|(_, scanout)| scanout.enabled)
            .map(|(number, scanout)| (number, scanout.rect))
            .ok_or(BindRefusal::NoScanout)
    }

    fn present(&mut self, rect: Rect, offset: u64) -> Result<(), CommandFailure> {
        self.execute(Command::TransferToHost2d {
            resource_id: RESOURCE_ID,
            rect,
            offset,
        })?;
        self.execute(Command::ResourceFlush {
            resource_id: RESOURCE_ID,
            rect,
        })?;
        Ok(())
    }

    fn execute(&mut self, command: Command<'_>) -> Result<[u8; RESPONSE_BYTES], CommandFailure> {
        let result = self.exchange(command);
        if result.is_err() {
            // Очередь и командные буферы переиспользуются только после подтверждённого сброса.
            self.queue = None;
        }
        result
    }

    fn exchange(&mut self, command: Command<'_>) -> Result<[u8; RESPONSE_BYTES], CommandFailure> {
        let queue = self.queue.as_mut().ok_or(CommandFailure::NotReady)?;
        let mut request = [0u8; RESPONSE_OFFSET];
        let length = command.encode(&mut request)?;
        self.commands
            .write(0, request.get(..length).ok_or(CommandFailure::Memory)?)?;
        self.commands.write(RESPONSE_OFFSET, &[0; RESPONSE_BYTES])?;
        let base = self.commands.physical_address();
        let token = queue.submit(
            &mut self.transport,
            &mut self.queue_memory,
            Buffer {
                address: base,
                length: u32::try_from(length).map_err(|_| CommandFailure::Memory)?,
            },
            Buffer {
                address: base + RESPONSE_OFFSET as u64,
                length: u32::try_from(command.response_bytes())
                    .map_err(|_| CommandFailure::Memory)?,
            },
        )?;
        let completion = self.await_completion()?;
        if completion.token != token {
            return Err(CommandFailure::UnexpectedCompletion);
        }
        let length = usize::try_from(completion.written).map_err(|_| CommandFailure::Memory)?;
        let mut response = [0u8; RESPONSE_BYTES];
        let written = response.get_mut(..length).ok_or(CommandFailure::Memory)?;
        self.commands.read(RESPONSE_OFFSET, written)?;
        command.check_response(written)?;
        Ok(response)
    }

    fn await_completion(&mut self) -> Result<Completion, CommandFailure> {
        loop {
            let queue = self.queue.as_mut().ok_or(CommandFailure::NotReady)?;
            if let Some(completion) =
                queue.take_used(&mut self.transport, &mut self.queue_memory)?
            {
                return Ok(completion);
            }
            self.line
                .wait(COMPLETION_TIMEOUT)
                .map_err(CommandFailure::Line)?;
            let causes = self.transport.interrupt_status()?;
            self.transport.acknowledge_interrupt(causes)?;
            self.line.ack().map_err(CommandFailure::Line)?;
        }
    }
}

fn open_slot(
    bus: &virtio_mmio::Client<SessionTransport>,
) -> Result<kernel_objects::Session<virtio_slot::Protocol>, BindRefusal> {
    // Шина освобождает слот после того, как обработает уход прошлого клиента, и до этого
    // отвечает `Busy`. Прошлым клиентом бывает и этот драйвер, отпустивший GPU с последней
    // сессией.
    let mut attempts = BUSY_ATTEMPTS;
    loop {
        match bus.open(DeviceType::Gpu as u32) {
            Err(ipc::CallError::Remote(VirtioMmioError::Busy)) if attempts > 1 => {
                attempts -= 1;
                let _ = Signal::create().map(|pause| pause.wait(SIGNALED, BUSY_PAUSE));
            }
            opened => {
                let slot =
                    opened.map_err(|error| error.fold(BindRefusal::BusCall, BindRefusal::Bus))?;
                return Ok(kernel_objects::Session::from_session(slot));
            }
        }
    }
}

fn start_offset(area: Area, stride: u64) -> Option<u64> {
    u64::from(area.y)
        .checked_mul(stride)?
        .checked_add(u64::from(area.x) * PIXEL_BYTES)
}
