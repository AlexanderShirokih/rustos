//! Production-цепочка userland: запуск первого userland-процесса с bootstrap-портом.

extern crate alloc;

use alloc::{sync::Arc, vec::Vec};
use core::{marker::PhantomData, num::NonZeroUsize};

use bootstrap_abi::{BootFdt, SystemSnapshot, bootstrap, hardware, system_info};
use capability::{
    Capability, CapabilityTarget, HandleId, HandleTable, IpcError as KernelIpcError, IrqControl,
    KernelIpcBuffer, MeteringGrant, Port, Reply, Resource, Rights, ThreadTransport, deadline_after,
    default_rights_for, interrupts_control, port_recv, port_send, release_delivered, runtime,
};
use drivers_common::services::interrupts::IrqLineSpan;
use ipc::{
    FrameBytes, Incoming, IntoWireValue, Listener, Outgoing, ReceivedResources, RecvError,
    ReplyChannel, Unsent,
    wire::{Cap, IpcError as WireError, MESSAGE_INLINE_MAX, MessageBuf, Session},
};
use kbase::{LockCell, MutexCell};
use klog::{info, warn};
use memory::{
    AccessMask, MemoryRegion, PAGE_SIZE, frame_allocator::FrameCount, page_round_up,
    physical_address::PageAlignedAddress,
};
use process::{UserImageFromModelError, user_image_parts_from_program};
use scheduler::{Priority, SchedulerService, UserProcessLaunch, UserProcessLaunchInfo};
use syscall::{IPC_BUFFER_MAX_CAPS, IpcBuffer, ObjectKind, Timeout, decode_tag, encode_tag, kind};
use userland::{ProgramDecodeError, decode_program};
use userland_image::{ImageDecodeError, component_name, decode};

use crate::{
    kernel_context::{BootDtb, UserlandImage},
    user_process::{SpawnUserError, UserProcessLauncher},
};

const SENDER_PORT_RIGHTS: Rights = Rights::from_bits_truncate(
    Rights::WRITE.bits() | Rights::TRANSFER.bits() | Rights::DUPLICATE.bits(),
);

const HARDWARE_BADGE: u64 = 1;
const SYSTEM_INFO_BADGE: u64 = 2;

/// Число страниц стека kernel-потока, исполняющего [`run_bootstrap`].
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

/// Capability-ресурсы и источники величин, которые сервер порта ядра выдаёт по своим контрактам.
pub struct BootstrapResources {
    pub irq_control: Arc<IrqControl>,
    pub image_region: Arc<MemoryRegion>,
    pub fdt_region: Arc<MemoryRegion>,
    /// Смещение блоба DTB от начала `fdt_region`.
    pub fdt_offset: u32,
    pub device_regions: Vec<Arc<MemoryRegion>>,
    /// Источник кап сессий `Hardware`.
    pub hardware_root: Capability,
    /// Источник кап сессий `SystemInfo`.
    pub system_info_root: Capability,
    pub system: SystemSources,
}

/// Владельцы величин снимка `SystemInfo`. Сервер читает их при каждом вызове `snapshot`.
pub struct SystemSources {
    pub scheduler: Arc<dyn SchedulerService>,
    pub frames: &'static (dyn FrameCount + Send + Sync),
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

/// Разбирает байты `image` и спавнит процесс. Поверх физбазы `image` выдаётся read-only регион
/// образа.
pub fn spawn_process(
    launcher: &dyn UserProcessLauncher,
    image: UserlandImage,
    dtb: BootDtb,
    device_regions: Vec<Arc<MemoryRegion>>,
    irq_lines: IrqLineSpan,
    system: SystemSources,
    user_va_end: usize,
) -> Result<BootstrapLaunch, BootstrapSpawnError> {
    let UserlandImage {
        bytes: blob,
        phys_base: image_phys,
    } = image;
    let image = decode(blob).map_err(BootstrapSpawnError::Image)?;
    let boot_entry = image.boot_entry();
    let program = decode_program(boot_entry.data()).map_err(BootstrapSpawnError::Program)?;
    let name = component_name(boot_entry.path());
    let parts =
        user_image_parts_from_program(&program, user_va_end).map_err(BootstrapSpawnError::Model)?;
    let user_image = parts.image();

    // Ядро принимает на порту, поэтому держит capability с READ. Стартовый
    // хэндл bootstrap-процесса - клиентский: отправка, перенос и минт лог-капы.
    let port = Port::new();
    let receiver = Capability::new(CapabilityTarget::Port(port.clone()), Rights::READ);
    let peer_handle = Capability::new(CapabilityTarget::Port(port.clone()), SENDER_PORT_RIGHTS);
    let hardware_root = Capability::new(CapabilityTarget::Port(port.clone()), SENDER_PORT_RIGHTS);
    let system_info_root =
        Capability::new(CapabilityTarget::Port(port.clone()), SENDER_PORT_RIGHTS);

    // Корневой Resource: полномочие на минтинг физпамяти.
    let root_resource = Resource::new(AccessMask::RW, 1 << 20);

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
            hardware_root,
            system_info_root,
            system,
        },
    })
}

/// Цикл сервера bootstrap-port: kernel-поток аллоцирует kernel-резидентный
/// IPC-буфер, строит kernel-транспорт и в бесконечном цикле обслуживает контракт `Bootstrap` и
/// контракты ролей, различая роль значком кадра. На кадр, который не разбирается, сервер
/// отвечает отказом и продолжает приём. `_receiver` - capability с `READ`, которая держит ядро
/// в множестве получателей порта, пока сервер работает. Поток поднимается со стеком
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
        // Вызыватель уже получил отказ разбора либо доставки ответа, и приём продолжается.
        let _ = match frame.resources.badge {
            HARDWARE_BADGE => hardware::dispatch(&mut server, frame),
            SYSTEM_INFO_BADGE => system_info::dispatch(&mut server, frame),
            _ => bootstrap::dispatch(&mut server, frame),
        };
    }
}

/// Отправляет серверу порта ядра пустой кадр и возвращается, когда сервер его принял. Возврат
/// `Ok` означает, что сервер обработал всё принятое до этого кадра. Сам пустой кадр сервер
/// отбрасывает.
pub fn drain_bootstrap_port(port: &Arc<Port>, timeout_ns: u64) -> Result<(), KernelIpcError> {
    let table = runtime()
        .current_handle_table()
        .ok_or(KernelIpcError::BadHandle)?;

    // Нулевой буфер содержит тег пустого кадра, без байт и без capability.
    let buffer: KernelIpcBuffer = Arc::new(MutexCell::new(IpcBuffer::zeroed()));

    let sender = ThreadTransport::new_kernel(buffer, table);
    // Кадр барьера идёт без значка - у него нет учёта отправителя.
    port_send(port, &sender, runtime(), Some(timeout_ns), None)
}

const ACQUIRE_FAILED: u32 = 1;
const NO_SUCH_WINDOW: u32 = 2;

struct BootstrapServer {
    table: Arc<MutexCell<HandleTable>>,
    resources: BootstrapResources,
}

struct KernelEntry<K: ObjectKind> {
    table: Arc<MutexCell<HandleTable>>,
    id: Option<HandleId>,
    kind: PhantomData<fn() -> K>,
}

impl BootstrapServer {
    fn vend<K: ObjectKind>(&self, cap: Capability) -> Result<KernelEntry<K>, u32> {
        // Таблица ядра хранит запись без статической метки вида. Вид называет операция
        // контракта, и выдача применяет его к номеру записи.
        let id = self
            .table
            .with_lock(|tbl| tbl.insert(cap))
            .map_err(|_| ACQUIRE_FAILED)?;

        Ok(KernelEntry {
            table: self.table.clone(),
            id: Some(id),
            kind: PhantomData,
        })
    }

    fn vend_root<K: ObjectKind>(&self, target: CapabilityTarget) -> Result<KernelEntry<K>, u32> {
        self.vend(Capability::new_with_default_rights(target))
    }
}

impl<K: ObjectKind> KernelEntry<K> {
    fn retype<L: ObjectKind>(mut self) -> KernelEntry<L> {
        KernelEntry {
            table: self.table.clone(),
            id: self.id.take(),
            kind: PhantomData,
        }
    }
}

impl<K: ObjectKind> IntoWireValue<BootstrapFrame> for KernelEntry<K> {
    type Read = Cap<K>;

    fn write_into(
        self,
        frame: &mut MessageBuf<BootstrapFrame>,
        field_id: u8,
    ) -> Result<(), WireError> {
        frame.write_handle(field_id, self)
    }
}

impl<K: ObjectKind> Drop for KernelEntry<K> {
    fn drop(&mut self) {
        let Some(id) = self.id.take() else {
            return;
        };
        if let Ok(drained) = self.table.with_lock(|tbl| tbl.remove(id)) {
            drained.release();
        }
    }
}

impl bootstrap::Service<BootstrapFrame> for BootstrapServer {
    fn log(&mut self, _frame: &BootstrapFrame, tag: &str, message: &str) {
        if tag.is_empty() {
            info!("{}", message);
        } else {
            info!(tag; "{}", message);
        }
    }

    fn acquire_userland_image(
        &mut self,
        _frame: &BootstrapFrame,
    ) -> impl IntoWireValue<BootstrapFrame, Read = Result<Cap<kind::MemoryRegion>, u32>> {
        self.vend_root::<kind::MemoryRegion>(CapabilityTarget::Memory(
            self.resources.image_region.clone(),
        ))
    }

    fn acquire_hardware(
        &mut self,
        _frame: &BootstrapFrame,
    ) -> impl IntoWireValue<BootstrapFrame, Read = Result<Session<hardware::Protocol>, u32>> {
        self.resources
            .hardware_root
            .duplicate(SENDER_PORT_RIGHTS, HARDWARE_BADGE)
            .map_err(|_| ACQUIRE_FAILED)
            .and_then(|cap| self.vend::<kind::Port>(cap))
            .map(Session::<hardware::Protocol, _>::from_handle)
    }

    fn acquire_system_info(
        &mut self,
        _frame: &BootstrapFrame,
    ) -> impl IntoWireValue<BootstrapFrame, Read = Result<Session<system_info::Protocol>, u32>>
    {
        self.resources
            .system_info_root
            .duplicate(SENDER_PORT_RIGHTS, SYSTEM_INFO_BADGE)
            .map_err(|_| ACQUIRE_FAILED)
            .and_then(|cap| self.vend::<kind::Port>(cap))
            .map(Session::<system_info::Protocol, _>::from_handle)
    }
}

impl hardware::Service<BootstrapFrame> for BootstrapServer {
    fn acquire_irq_control(
        &mut self,
        _frame: &BootstrapFrame,
    ) -> impl IntoWireValue<BootstrapFrame, Read = Result<Cap<kind::IrqControl>, u32>> {
        self.vend_root::<kind::IrqControl>(CapabilityTarget::IrqControl(
            self.resources.irq_control.clone(),
        ))
    }

    fn acquire_boot_fdt(
        &mut self,
        _frame: &BootstrapFrame,
    ) -> impl IntoWireValue<BootstrapFrame, Read = Result<BootFdt, u32>> {
        self.vend_root::<kind::MemoryRegion>(CapabilityTarget::Memory(
            self.resources.fdt_region.clone(),
        ))
        .map(|region| BootFdt {
            region,
            offset: self.resources.fdt_offset,
        })
    }

    fn acquire_device_memory(
        &mut self,
        _frame: &BootstrapFrame,
        index: u32,
    ) -> impl IntoWireValue<BootstrapFrame, Read = Result<Cap<kind::MemoryRegion>, u32>> {
        self.resources
            .device_regions
            .get(index as usize)
            .ok_or(NO_SUCH_WINDOW)
            .and_then(|region| {
                self.vend_root::<kind::MemoryRegion>(CapabilityTarget::Memory(region.clone()))
            })
    }
}

impl system_info::Service<BootstrapFrame> for BootstrapServer {
    fn snapshot(&mut self, _frame: &BootstrapFrame) -> SystemSnapshot {
        let system = &self.resources.system;

        SystemSnapshot {
            cpus: u32::try_from(system.scheduler.cpu_count()).unwrap_or(u32::MAX),
            managed_frames: system.frames.managed_frames() as u64,
            free_frames: system.frames.free_frames() as u64,
            frame_size: u32::try_from(PAGE_SIZE.get()).unwrap_or(u32::MAX),
        }
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

#[derive(Default)]
struct BootstrapFrame {
    badge: u64,
    reply: Option<KernelReply>,
    entries: [Option<KernelEntry<kind::Any>>; IPC_BUFFER_MAX_CAPS],
}

impl BootstrapFrame {
    fn numbers(&self) -> impl Iterator<Item = u32> + '_ {
        self.entries
            .iter()
            .flatten()
            .filter_map(|entry| entry.id)
            .map(|id| id.raw().get())
    }

    fn transferred(mut self) {
        for entry in self.entries.iter_mut().flatten() {
            entry.id = None;
        }
    }
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
    type Handle<K: syscall::ObjectKind> = KernelEntry<K>;
    type Reply = KernelReply;

    fn take_handle<K: syscall::ObjectKind>(&mut self, index: u8) -> Option<KernelEntry<K>> {
        let entry = self.entries.get_mut(usize::from(index))?.take()?;
        Some(entry.retype())
    }

    fn put_handle<K: syscall::ObjectKind>(&mut self, handle: KernelEntry<K>) -> Option<u8> {
        let index = self.entries.iter().position(Option::is_none)?;
        *self.entries.get_mut(index)? = Some(handle.retype());
        u8::try_from(index).ok()
    }

    fn reply_channel(_handle: KernelEntry<kind::Reply>) -> Option<KernelReply> {
        // Сервер порта ядра не принимает capability, и запись канала ответа в его кадре
        // не встречается.
        None
    }

    fn take_reply(&mut self) -> Option<KernelReply> {
        self.reply.take()
    }
}

impl ReplyChannel for KernelReply {
    type Resources = BootstrapFrame;

    fn send(
        mut self,
        frame: Outgoing<'_, BootstrapFrame>,
    ) -> Result<(), Unsent<'_, BootstrapFrame>> {
        let Some(reply) = self.reply.take() else {
            return Err((WireError::PeerClosed, frame));
        };
        let delivered = self
            .buffer
            .with_lock(|buf| store_kernel_buffer(buf, &frame))
            .and_then(|()| {
                reply
                    .reply(&ThreadTransport::new_kernel(
                        self.buffer.clone(),
                        self.table.clone(),
                    ))
                    .map_err(map_kernel_error)
            });
        match delivered {
            Ok(()) => {
                frame.into_resources().transferred();
                Ok(())
            }
            Err(error) => {
                reply.cancel();
                Err((error, frame))
            }
        }
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
        let deadline_ns =
            deadline_after(runtime(), (wait != Timeout::INFINITE).then_some(wait.raw()));
        // Роль получателя держит capability сервера всё время его работы.
        let reply = loop {
            match port_recv(&self.port, &receiver, runtime(), deadline_ns, None) {
                Ok(reply) => break reply,
                Err(KernelIpcError::ShouldWait) => {}
                Err(KernelIpcError::SenderGone) => {
                    return Err(RecvError::SenderGone(
                        self.buffer.with_lock(|buf| buf.badge),
                    ));
                }
                Err(KernelIpcError::Timeout) => return Err(RecvError::Timeout),
                Err(_) => return Err(RecvError::Closed),
            }
        };
        // Разрушение ресурсов кадра, который не принят, отменяет ответ его вызывателю.
        let resources = BootstrapFrame {
            badge: self.buffer.with_lock(|buf| buf.badge),
            reply: reply.map(|reply| KernelReply {
                reply: Some(reply),
                buffer: self.buffer.clone(),
                table: self.table.clone(),
            }),
            entries: Default::default(),
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
    frame: &Outgoing<'_, BootstrapFrame>,
) -> Result<(), WireError> {
    let bytes = frame.as_bytes();
    buf.data
        .get_mut(..bytes.len())
        .ok_or(WireError::FrameOverflow)?
        .copy_from_slice(bytes);
    let mut ncaps = 0;
    for (slot, number) in buf.caps.iter_mut().zip(frame.resources().numbers()) {
        *slot = number;
        ncaps += 1;
    }
    buf.tag = encode_tag(bytes.len(), ncaps);
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
