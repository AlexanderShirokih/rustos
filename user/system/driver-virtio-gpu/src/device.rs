//! Привязка GPU через слот шины.

use display_abi::{Area, DisplayDimension, DisplayError, PhysicalSize, SURFACE_PIXEL_BYTES};
use ipc::wire::IpcError;
use kernel_objects::{IrqLine, Mapping, SessionTransport};
use runtime::{Timeout, log_fmt};
use virtio::{
    Buffer, Completion, DeviceMemory, DeviceType, QUEUE_REGION_BYTES, QueueError, SplitQueue,
    Transport, TransportError,
    gpu::{
        Command, DisplayInfo, EDID_RESPONSE_BYTES, FEATURE_EDID, Format, GpuError, ImageSize, Rect,
    },
};
use virtio_driver::{
    CompletionFailure, DmaRegion, SlotFailure, SlotRegisters, await_completion, open_slot,
};
use virtio_mmio_abi::{SlotError, VirtioMmioError, virtio_mmio};

const SLOT_CALL_TIMEOUT: Timeout = Timeout::from_secs(2);
const COMPLETION_TIMEOUT: Timeout = Timeout::from_secs(2);
const CONTROL_QUEUE: u16 = 0;
const CONTROL_QUEUE_SIZE: u16 = 2;
const RESOURCE_ID: u32 = 1;
const RESPONSE_OFFSET: usize = 512;
const RESPONSE_BYTES: usize = EDID_RESPONSE_BYTES;
const COMMANDS_REGION_BYTES: u64 = (RESPONSE_OFFSET + RESPONSE_BYTES) as u64;
const PIXEL_BYTES: u64 = SURFACE_PIXEL_BYTES as u64;

const TRANSFER_BUFFER_MAX_BYTES: u64 = 2 * 1024 * 1024;
const TRANSFER_HEAD_BYTES: u64 = 8;

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
    Completion(CompletionFailure),
    Memory,
    NotReady,
    UnexpectedCompletion,
}

pub(crate) struct Gpu {
    device: Device,
    dimension: DisplayDimension,
    physical_size: Option<PhysicalSize>,
    scanout: u32,
}

struct Device {
    transport: Transport<SlotRegisters>,
    line: IrqLine,
    queue: Option<SplitQueue>,
    queue_memory: DmaRegion,
    commands: DmaRegion,
    transfer: Option<TransferBuffer>,
    edid: bool,
}

struct TransferBuffer {
    region: DmaRegion,
    rows: u32,
    length: u32,
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

impl From<CompletionFailure> for CommandFailure {
    fn from(failure: CompletionFailure) -> Self {
        Self::Completion(failure)
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
            Ok((scanout, dimension)) => {
                let physical_size = device.physical_size(scanout);
                Ok(Self {
                    device,
                    dimension,
                    physical_size,
                    scanout,
                })
            }
            Err(refusal) => {
                device.release();
                Err(refusal)
            }
        }
    }

    pub(crate) fn dimension(&self) -> DisplayDimension {
        self.dimension
    }

    pub(crate) fn physical_size(&self) -> Option<PhysicalSize> {
        self.physical_size
    }

    pub(crate) fn present(&mut self, surface: &Mapping, area: Area) -> Result<(), DisplayError> {
        self.dimension.check_area(area)?;

        let area = if self.device.queue.is_none() {
            self.device
                .initialize()
                .and_then(|()| self.device.attach_resource(self.scanout, self.dimension))
                .map_err(|refusal| {
                    log_fmt(format_args!("gpu recovery refused: {refusal:?}"));
                    DisplayError::Hardware
                })?;
            Area {
                x: 0,
                y: 0,
                width: self.dimension.width,
                height: self.dimension.height,
            }
        } else {
            area
        };

        let stride = self.dimension.surface_stride();
        let mut top = 0;
        while top < area.height {
            let transfer = self
                .device
                .transfer
                .as_mut()
                .ok_or(DisplayError::Hardware)?;
            let chunk = Area {
                y: area.y + top,
                height: (area.height - top).min(transfer.rows),
                ..area
            };

            let offset = transfer
                .stage(surface, chunk, stride)
                .ok_or(DisplayError::OutOfBounds)?;
            self.device
                .transfer_to_host(chunk, offset)
                .map_err(refused)?;

            top += chunk.height;
        }

        self.device.flush(area).map_err(refused)
    }

    pub(crate) fn release(self) {
        self.device.release();
    }
}

impl Device {
    fn open(bus: &virtio_mmio::Client<SessionTransport>) -> Result<Self, BindRefusal> {
        let slot = open_slot(bus, DeviceType::Gpu)
            .map_err(|error| error.fold(BindRefusal::BusCall, BindRefusal::Bus))?
            .client()
            .with_wait(SLOT_CALL_TIMEOUT);
        let line = slot
            .irq()
            .map_err(|error| error.fold(BindRefusal::BusCall, BindRefusal::Line))?;
        let transport = Transport::probe(SlotRegisters::new(slot), DeviceType::Gpu)
            .map_err(BindRefusal::Transport)?;
        let queue_memory = DmaRegion::new(QUEUE_REGION_BYTES as u64)?;
        let commands = DmaRegion::new(COMMANDS_REGION_BYTES)?;
        let mut device = Self {
            transport,
            line: IrqLine::from(line),
            queue: None,
            queue_memory,
            commands,
            transfer: None,
            edid: false,
        };
        if let Err(refusal) = device.initialize() {
            device.release();
            return Err(refusal);
        }
        Ok(device)
    }

    fn initialize(&mut self) -> Result<(), BindRefusal> {
        self.queue = None;
        let accepted = self
            .transport
            .negotiate(FEATURE_EDID)
            .map_err(BindRefusal::Transport)?;
        self.edid = accepted & FEATURE_EDID != 0;
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
        self.transfer = Some(TransferBuffer::new(dimension)?);
        self.attach_resource(scanout, dimension)?;
        Ok((scanout, dimension))
    }

    fn attach_resource(
        &mut self,
        scanout: u32,
        dimension: DisplayDimension,
    ) -> Result<(), BindRefusal> {
        let transfer = self.transfer.as_ref().ok_or(CommandFailure::NotReady)?;
        let backing = [Buffer {
            address: transfer.region.physical_address(),
            length: transfer.length,
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
            core::mem::forget((self.queue_memory, self.commands, self.transfer));
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

    fn physical_size(&mut self, scanout: u32) -> Option<PhysicalSize> {
        if !self.edid {
            return None;
        }

        let size = self
            .execute(Command::GetEdid {
                scanout_id: scanout,
            })
            .and_then(|response| {
                ImageSize::from_edid_response(&response).map_err(CommandFailure::Gpu)
            });

        match size {
            Ok(size) => size.map(|size| PhysicalSize {
                width_mm: size.width_mm,
                height_mm: size.height_mm,
            }),
            Err(failure) => {
                log_fmt(format_args!("edid refused: {failure:?}"));
                None
            }
        }
    }

    fn transfer_to_host(&mut self, area: Area, offset: u64) -> Result<(), CommandFailure> {
        self.execute(Command::TransferToHost2d {
            resource_id: RESOURCE_ID,
            rect: Rect {
                x: area.x,
                y: area.y,
                width: area.width,
                height: area.height,
            },
            offset,
        })?;
        Ok(())
    }

    fn flush(&mut self, area: Area) -> Result<(), CommandFailure> {
        self.execute(Command::ResourceFlush {
            resource_id: RESOURCE_ID,
            rect: Rect {
                x: area.x,
                y: area.y,
                width: area.width,
                height: area.height,
            },
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
        let request = Buffer {
            address: base,
            length: u32::try_from(length).map_err(|_| CommandFailure::Memory)?,
        };
        let response = Buffer {
            address: base + RESPONSE_OFFSET as u64,
            length: u32::try_from(command.response_bytes()).map_err(|_| CommandFailure::Memory)?,
        };
        let token = queue.submit(
            &mut self.transport,
            &mut self.queue_memory,
            &[request],
            &[response],
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
        let queue = self.queue.as_mut().ok_or(CommandFailure::NotReady)?;
        Ok(await_completion(
            queue,
            &mut self.transport,
            &mut self.queue_memory,
            &self.line,
            COMPLETION_TIMEOUT,
        )?)
    }
}

impl TransferBuffer {
    fn new(dimension: DisplayDimension) -> Result<Self, BindRefusal> {
        let geometry = BindRefusal::Geometry(dimension.width, dimension.height);

        let rows = (TRANSFER_BUFFER_MAX_BYTES - TRANSFER_HEAD_BYTES)
            .checked_div(dimension.surface_stride())
            .map(|rows| rows.min(u64::from(dimension.height)))
            .and_then(|rows| u32::try_from(rows).ok())
            .filter(|rows| *rows != 0)
            .ok_or(geometry)?;
        let length = TRANSFER_HEAD_BYTES + u64::from(rows) * dimension.surface_stride();

        Ok(Self {
            region: DmaRegion::new(length)?,
            rows,
            length: u32::try_from(length).map_err(|_| geometry)?,
        })
    }

    fn stage(&mut self, surface: &Mapping, chunk: Area, stride: u64) -> Option<u64> {
        let left = u64::from(chunk.x) * PIXEL_BYTES;
        let row = usize::try_from(u64::from(chunk.width) * PIXEL_BYTES).ok()?;
        let offset = TRANSFER_HEAD_BYTES + left;

        for line in 0..chunk.height {
            let source = (u64::from(chunk.y) + u64::from(line))
                .checked_mul(stride)?
                .checked_add(left)?;
            let destination = offset + u64::from(line) * stride;

            self.region
                .copy_from(
                    usize::try_from(destination).ok()?,
                    surface,
                    usize::try_from(source).ok()?,
                    row,
                )
                .ok()?;
        }

        Some(offset)
    }
}

fn refused(failure: CommandFailure) -> DisplayError {
    log_fmt(format_args!("present refused by the device: {failure:?}"));
    DisplayError::Hardware
}
