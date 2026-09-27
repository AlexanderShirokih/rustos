//! Production-цепочка userland: запуск первого userland-процесса с bootstrap-портом.

extern crate alloc;

use alloc::{sync::Arc, vec::Vec};
use core::num::NonZeroUsize;

use bootstrap_abi::{BootFdt, bootstrap};
use capability::{
    Capability, CapabilityTarget, HandleTable, IpcError as KernelIpcError, IrqControl,
    KernelIpcBuffer, MeteringGrant, Port, Reply, Resource, Rights, ThreadTransport,
    default_rights_for, interrupts_control, port_recv, port_send, release_delivered, runtime,
};
use collections::{LockCell, MutexCell};
use drivers_common::services::interrupts::IrqLineSpan;
use ipc::{
    FrameBytes, Incoming, Listener, ReceivedResources, RecvError, ReplyChannel,
    wire::{Cap, IpcError as WireError, MESSAGE_INLINE_MAX, Str},
};
use klog::{info, warn};
use memory::{
    AccessMask, MemoryRegion, page_round_up,
    physical_address::{PageAlignedAddress, PhysicalAddress},
};
use process::{UserImageFromModelError, user_image_parts_from_entry};
use scheduler::{Priority, UserProcessLaunch, UserProcessLaunchInfo};
use syscall::{IpcBuffer, Timeout, decode_tag, encode_tag};
use userland::EntryView;
use userland_image::{ImageDecodeError, ProgramDecodeError, component_name, decode};

use crate::{
    kernel_context::BootDtb,
    user_process::{SpawnUserError, UserProcessLauncher},
};

const ROOT_RESOURCE_SPAN: NonZeroUsize = NonZeroUsize::new(usize::MAX & !0xFFF).unwrap();

/// Число страниц стека kernel-потока, исполняющего [`run_bootstrap`]. Обобщённый
/// диспетчер `ipc` держит на стеке два инлайн-кадра контракта разом, и на
/// операции `acquire_boot_fdt` замеренный high-water доходит до 18392 байт.
pub const SERVER_STACK_PAGES: usize = 8;

/// Запущенный bootstrap-процесс.
pub struct BootstrapLaunch {
    pub port: Arc<Port>,
    /// Пока capability получателя жива, порт считает ядро возможным
    /// получателем.
    pub receiver: Capability,
    pub info: UserProcessLaunchInfo,
    pub resources: BootstrapResources,
}

/// Capability-ресурсы, вендимые bootstrap-сервером корневому процессу.
pub struct BootstrapResources {
    pub irq_control: Arc<IrqControl>,
    pub image_region: Arc<MemoryRegion>,
    pub fdt_region: Arc<MemoryRegion>,
    /// Смещение блоба DTB от начала `fdt_region`.
    pub fdt_offset: u32,
    pub device_regions: Vec<Arc<MemoryRegion>>,
}

/// Ошибки запуска bootstrap-процесса.
#[derive(Debug)]
pub enum BootstrapSpawnError {
    Image(ImageDecodeError),
    Program(ProgramDecodeError),
    Model(UserImageFromModelError),
    Spawn(SpawnUserError),
    /// База образа не выровнена на страницу либо размер не округляется до страницы
    /// в пределах адресного пространства.
    ImageGeometry,
    /// Размер DTB не округляется до страницы в пределах адресного пространства.
    FdtGeometry,
    /// Контроллер прерываний для IRQ-capability не установлен.
    InterruptsUnavailable,
}

/// Разбирает `blob` и спавнит процесс. `image_phys` - физбаза `blob` в initrd,
/// поверх которой выдаётся read-only регион образа.
pub fn spawn_process(
    launcher: &dyn UserProcessLauncher,
    blob: &'static [u8],
    image_phys: PhysicalAddress,
    dtb: BootDtb,
    device_regions: Vec<Arc<MemoryRegion>>,
    irq_lines: IrqLineSpan,
    user_va_end: usize,
) -> Result<BootstrapLaunch, BootstrapSpawnError> {
    let image = decode(blob).map_err(BootstrapSpawnError::Image)?;
    let program = image
        .boot_entry()
        .program()
        .map_err(BootstrapSpawnError::Program)?;
    let name = component_name(program.path());
    let parts =
        user_image_parts_from_entry(&program, user_va_end).map_err(BootstrapSpawnError::Model)?;
    let user_image = parts.image();

    // Ядро принимает на порту, поэтому держит capability с READ. Стартовый
    // хэндл bootstrap-процесса - клиентский: отправка, перенос и минт лог-капы.
    let port = Port::new();
    let receiver = Capability::new(CapabilityTarget::Port(port.clone()), Rights::READ);
    let peer_handle = Capability::new(
        CapabilityTarget::Port(port.clone()),
        Rights::WRITE | Rights::TRANSFER | Rights::DUPLICATE,
    );

    // Корневой Resource: полномочие на минтинг физпамяти.
    let root_resource = Resource::new(
        PageAlignedAddress::ZERO,
        ROOT_RESOURCE_SPAN,
        AccessMask::RW,
        1 << 20,
    );

    let interrupts = interrupts_control()
        .ok_or(BootstrapSpawnError::InterruptsUnavailable)?
        .clone();
    let irq_control = IrqControl::new(interrupts, irq_lines.first.raw(), irq_lines.last.raw());

    // Read-only Normal-регион поверх байт образа; размер округлён до страницы
    // для маппинга. blob непуст (decode прошёл), поэтому размер ненулевой.
    // Потребитель читает регион с нулевого смещения, поэтому база обязана
    // совпадать с началом страницы.
    let image_base =
        PageAlignedAddress::new(image_phys).ok_or(BootstrapSpawnError::ImageGeometry)?;
    let image_size = NonZeroUsize::new(blob.len())
        .and_then(page_round_up)
        .ok_or(BootstrapSpawnError::ImageGeometry)?;
    let image_region = Arc::new(MemoryRegion::create_physical_normal(
        image_base,
        image_size,
        AccessMask::R,
    ));

    // Read-only Normal-регион поверх страниц с байтами DTB. Память DTB зарезервирована раскладкой
    // на весь срок ядра.
    let fdt = dtb.blob_region().ok_or(BootstrapSpawnError::FdtGeometry)?;
    let fdt_offset = u32::try_from(fdt.offset).map_err(|_| BootstrapSpawnError::FdtGeometry)?;
    let fdt_region = Arc::new(MemoryRegion::create_physical_normal(
        fdt.base,
        fdt.size,
        AccessMask::R,
    ));

    let launch = UserProcessLaunch::new(peer_handle);

    let info = launcher
        .spawn_user_process_with_launch(name, &user_image, Priority::normal(), 2, launch)
        .map_err(BootstrapSpawnError::Spawn)?;

    // Корневой Resource - метеринг-ресурс bootstrap-процесса; ставится до старта scheduler.
    // Корень дерева процессов держит его с полным набором прав.
    let root_grant_rights =
        default_rights_for(&CapabilityTarget::Resource(Arc::clone(&root_resource)));
    info.process_object.set_metering(MeteringGrant {
        resource: root_resource,
        rights: root_grant_rights,
    });

    Ok(BootstrapLaunch {
        port,
        receiver,
        info,
        resources: BootstrapResources {
            irq_control,
            image_region,
            fdt_region,
            fdt_offset,
            device_regions,
        },
    })
}

/// Цикл сервера bootstrap-port: kernel-поток аллоцирует kernel-резидентный
/// IPC-буфер, строит kernel-транспорт и в бесконечном цикле обслуживает контракт
/// `Bootstrap`. `_receiver` - capability с `READ`: пока сервер работает, она
/// держит ядро в множестве получателей порта. Поток поднимается со стеком
/// [`SERVER_STACK_PAGES`].
pub fn run_bootstrap(port: &Arc<Port>, _receiver: Capability, resources: BootstrapResources) {
    let Some(table) = runtime().current_handle_table() else {
        warn!("bootstrap: no kernel handle-table; server not started");
        return;
    };

    let buffer: KernelIpcBuffer = Arc::new(MutexCell::new(IpcBuffer::zeroed()));
    let listener = KernelPort {
        port: port.clone(),
        buffer,
        table: table.clone(),
    };
    let mut server = BootstrapServer { table, resources };
    let mut bytes = [0u8; MESSAGE_INLINE_MAX];

    loop {
        let frame = match listener.recv(&mut bytes, Timeout::INFINITE) {
            Ok(frame) => frame,
            // Сервер не держит сессий по значкам. Отброшенный кадр относится к одному вызову, и
            // его вызыватель уже получил отказ. Оба исхода не прерывают приём.
            Err(RecvError::SenderGone(_) | RecvError::Discarded) => continue,
            Err(e) => {
                warn!("bootstrap: receive failed: {:?}", e);
                return;
            }
        };
        if frame.bytes.is_empty() {
            info!("bootstrap: port closed, exiting");
            return;
        }
        match bootstrap::dispatch(&mut server, frame) {
            Ok(()) => {}
            // Отказ доставки ответа ушедшему вызывателю порт не закрывает.
            Err(WireError::PeerClosed) => {
                warn!("bootstrap: reply delivery failed: caller gone");
            }
            Err(e) => {
                warn!("bootstrap: dispatch failed: {:?}", e);
                return;
            }
        }
    }
}

/// Закрывает bootstrap-port со стороны ядра пустым кадром.
/// Возврат `Ok` означает, что сервер обработал всё принятое до закрытия.
pub fn close_bootstrap_port(port: &Arc<Port>, timeout_ns: u64) -> Result<(), KernelIpcError> {
    let table = runtime()
        .current_handle_table()
        .ok_or(KernelIpcError::BadHandle)?;

    // Нулевой буфер содержит тег пустого кадра, без байт и без capability.
    let buffer: KernelIpcBuffer = Arc::new(MutexCell::new(IpcBuffer::zeroed()));

    let sender = ThreadTransport::new_kernel(buffer, table);
    // Кадр закрытия идёт без значка - у него нет учёта отправителя.
    port_send(port, &sender, runtime(), Some(timeout_ns), None)
}

const ACQUIRE_FAILED: u32 = 1;
const NO_SUCH_WINDOW: u32 = 2;

struct BootstrapServer {
    table: Arc<MutexCell<HandleTable>>,
    resources: BootstrapResources,
}

impl BootstrapServer {
    fn vend<K: syscall::ObjectKind>(&self, target: CapabilityTarget) -> Result<Cap<K>, u32> {
        // Таблица ядра хранит запись без статической метки вида. Вид называет операция
        // контракта, и выдача применяет его к номеру записи.
        let cap = Capability::new_with_default_rights(target);
        let id = self
            .table
            .with_lock(|tbl| tbl.insert(cap))
            .map_err(|_| ACQUIRE_FAILED)?;

        Ok(Cap::from_handle(syscall::Handle::from_abi(id.raw())))
    }
}

impl bootstrap::Service<BootstrapFrame> for BootstrapServer {
    fn log(
        &mut self,
        _frame: &BootstrapFrame,
        tag: Str<{ bootstrap_abi::LOG_TAG_MAX }>,
        message: Str<{ bootstrap_abi::LOG_MESSAGE_MAX }>,
    ) {
        if tag.as_str().is_empty() {
            info!("{}", message.as_str());
        } else {
            info!(tag.as_str(); "{}", message.as_str());
        }
    }

    fn acquire_irq_control(
        &mut self,
        _frame: &BootstrapFrame,
    ) -> Result<Cap<syscall::kind::IrqControl>, u32> {
        self.vend(CapabilityTarget::IrqControl(
            self.resources.irq_control.clone(),
        ))
    }

    fn acquire_userland_image(
        &mut self,
        _frame: &BootstrapFrame,
    ) -> Result<Cap<syscall::kind::MemoryRegion>, u32> {
        self.vend(CapabilityTarget::Memory(
            self.resources.image_region.clone(),
        ))
    }

    fn acquire_boot_fdt(&mut self, _frame: &BootstrapFrame) -> Result<BootFdt, u32> {
        let region = self.vend(CapabilityTarget::Memory(self.resources.fdt_region.clone()))?;

        Ok(BootFdt {
            region,
            offset: self.resources.fdt_offset,
        })
    }

    fn acquire_device_memory(
        &mut self,
        _frame: &BootstrapFrame,
        index: u32,
    ) -> Result<Cap<syscall::kind::MemoryRegion>, u32> {
        let region = self
            .resources
            .device_regions
            .get(index as usize)
            .ok_or(NO_SUCH_WINDOW)?
            .clone();
        self.vend(CapabilityTarget::Memory(region))
    }
}

fn map_kernel_error(error: KernelIpcError) -> WireError {
    match error {
        KernelIpcError::Timeout => WireError::Timeout,
        KernelIpcError::BufferTooSmall => WireError::Truncated,
        KernelIpcError::MessageTooBig => WireError::FrameOverflow,
        KernelIpcError::ShouldWait
        | KernelIpcError::SenderGone
        | KernelIpcError::PeerClosed
        | KernelIpcError::BadHandle
        | KernelIpcError::WrongType
        | KernelIpcError::AccessDenied
        | KernelIpcError::Canceled
        | KernelIpcError::OutOfHandles
        | KernelIpcError::ResourceExhausted
        | KernelIpcError::Revoked
        | KernelIpcError::AlreadyBound => WireError::PeerClosed,
    }
}

struct BootstrapFrame {
    reply: Option<KernelReply>,
}

struct KernelReply {
    reply: Option<Arc<Reply>>,
    buffer: KernelIpcBuffer,
    table: Arc<MutexCell<HandleTable>>,
}

struct KernelPort {
    port: Arc<Port>,
    buffer: KernelIpcBuffer,
    table: Arc<MutexCell<HandleTable>>,
}

impl ReceivedResources for BootstrapFrame {
    type Handle<K: syscall::ObjectKind> = core::convert::Infallible;
    type Reply = KernelReply;

    fn take_handle<K: syscall::ObjectKind>(&mut self, _index: u8) -> Option<Self::Handle<K>> {
        None
    }

    fn reply_channel(handle: Self::Handle<syscall::kind::Reply>) -> Option<KernelReply> {
        match handle {}
    }

    fn take_reply(&mut self) -> Option<KernelReply> {
        self.reply.take()
    }
}

impl ReplyChannel for KernelReply {
    fn send(mut self, bytes: &[u8], handles: &[u32]) -> Result<(), WireError> {
        let reply = self.reply.take().ok_or(WireError::PeerClosed)?;
        let delivered = self
            .buffer
            .with_lock(|buf| store_kernel_buffer(buf, bytes, handles))
            .and_then(|()| {
                reply
                    .reply(&ThreadTransport::new_kernel(
                        self.buffer.clone(),
                        self.table.clone(),
                    ))
                    .map_err(map_kernel_error)
            });
        if delivered.is_err() {
            reply.cancel();
        }
        delivered
    }
}

impl Drop for KernelReply {
    fn drop(&mut self) {
        if let Some(reply) = self.reply.take() {
            reply.cancel();
        }
    }
}

impl Listener for KernelPort {
    type Resources = BootstrapFrame;

    fn recv<'b>(
        &self,
        into: &'b mut FrameBytes,
        wait: Timeout,
    ) -> Result<Incoming<'b, BootstrapFrame>, RecvError> {
        let receiver = ThreadTransport::new_kernel(self.buffer.clone(), self.table.clone());
        let timeout_ns = (wait != Timeout::INFINITE).then_some(wait.raw());
        // Роль получателя держит capability сервера всё время его работы.
        let reply = match port_recv(&self.port, &receiver, runtime(), timeout_ns, None) {
            Ok(reply) => reply,
            Err(KernelIpcError::SenderGone) => {
                return Err(RecvError::SenderGone(
                    self.buffer.with_lock(|buf| buf.badge),
                ));
            }
            Err(KernelIpcError::Timeout) => return Err(RecvError::Timeout),
            Err(_) => return Err(RecvError::Closed),
        };
        // Разрушение ресурсов кадра, который не принят, отменяет ответ его вызывателю.
        let resources = BootstrapFrame {
            reply: reply.map(|reply| KernelReply {
                reply: Some(reply),
                buffer: self.buffer.clone(),
                table: self.table.clone(),
            }),
        };
        let Some(bytes) = self.buffer.with_lock(|buf| load_kernel_buffer(buf, into)) else {
            // Вызыватель получает отказ разрушением `resources` только после закрытия capability
            // кадра.
            let _ = release_delivered(&receiver);
            return Err(RecvError::Discarded);
        };
        Ok(Incoming { bytes, resources })
    }
}

fn store_kernel_buffer(
    buf: &mut IpcBuffer,
    bytes: &[u8],
    handles: &[u32],
) -> Result<(), WireError> {
    if bytes.len() > buf.data.len() || handles.len() > buf.caps.len() {
        return Err(WireError::FrameOverflow);
    }
    buf.data
        .get_mut(..bytes.len())
        .ok_or(WireError::FrameOverflow)?
        .copy_from_slice(bytes);
    for (slot, &handle) in buf.caps.iter_mut().zip(handles) {
        *slot = handle;
    }
    buf.tag = encode_tag(bytes.len(), handles.len());
    Ok(())
}

fn load_kernel_buffer<'b>(buf: &IpcBuffer, into: &'b mut FrameBytes) -> Option<&'b [u8]> {
    let decoded = decode_tag(buf.tag);
    if decoded.ncaps > 0 {
        return None;
    }
    let len = decoded.len.min(buf.data.len());
    let bytes = into.get_mut(..len)?;
    bytes.copy_from_slice(buf.data.get(..len)?);
    Some(bytes)
}
