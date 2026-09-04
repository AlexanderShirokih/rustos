//! Владелец политики загрузки: монтирует статически связанный корень, разматывает из файлов
//! ступень 1 транзакцией супервизора и прививает маршрутизатору имён сессии её файловых
//! систем.
//!
//! Таблица монтирований едет записью каталога программы: на её чтении корень ещё не
//! смонтирован, и каталога у компонента нет. Корень приходит слотом по имени экземпляра, а
//! всё остальное дерево - сессиями компонентов, которые поднял сам `init`.
//!
//! Уход маршрутизатора уносит дерево имён и снимает держателя его control-слота, и свежая
//! incarnation `init` применяет таблицу заново. Ступень 1 переживает смену incarnation: её
//! набор принадлежит композиции, и повторный `commit` отвечает `Duplicate`.

#![no_std]
#![no_main]
#![allow(unsafe_code)]

runtime::process_heap!();

extern crate alloc;

use alloc::{
    format,
    string::{String, ToString},
    vec,
    vec::Vec,
};
use core::{
    fmt::{Display, Formatter, Result as FmtResult},
    panic::PanicInfo,
};

use component_abi::supervisor_gone;
use component_client::{ReceivedStartup, StartupError, component_manifest};
use filesystem_abi::{
    AccessMode, BOOT_MOUNT_TABLE_PATH, DirCursor, DirEntry, FileSystemError, MountTable,
    NodeDescriptor, NodeKind, OpenFlags, Opened, PATH_MAX, Path, decode_mount_table, file_system,
    namespace_admin,
};
use ipc::{
    Transport as _,
    wire::{Bytes, IpcError, Str},
};
use kernel_objects::{Mapping, MemoryRegion, OwnedHandle};
use runtime::{PortTransport, Timeout, UserMemFlags, install_log, log_fmt, log_line, thread_exit};
use supervisor_abi::{ExtendError, INSTANCE_NAME_MAX, PLAN_RECORD_MAX, supervisor};
use syscall::Handle;
use userland_image::{BOOT_STAGE_ONE_DIRECTORY, COMPONENT_PROGRAM_NAME, decode, decode_plan};

const LOG_TAG: &str = "init";

const CALL_TIMEOUT: Timeout = Timeout::from_secs(2);

const ROOT_FILE_SYSTEM: &str = "filesystem-imagefs";

const BUFFER_SIZE: u32 = 4096;

const SUPERVISOR_LOST_EXIT_CODE: u64 = 7;

const CONTROL_PORT_FAILED_EXIT_CODE: u64 = 9;

component_manifest! {
    struct InitStartup {
        slots: {
            image: userland,
            root: service(protocol = "FileSystem", instance = "filesystem-imagefs"),
            mounts: control(protocol = "NamespaceAdmin", instance = "namespace-router"),
            system: supervisor,
        },
    }
}

#[derive(Debug)]
enum Error {
    Runtime(kernel_objects::Error),
    Ipc(IpcError),
    Start,
    MountTable,
    Mount,
    StageOne,
}

type Result<T> = core::result::Result<T, Error>;

struct Reading {
    files: file_system::Client<PortTransport>,
    buffer: Mapping,
    _region: MemoryRegion,
    _session: OwnedHandle,
}

/// `startup` приходит в x0 как HandleId одноразового startup-порта.
#[unsafe(no_mangle)]
pub extern "C" fn _start(startup: Handle) -> ! {
    match run(startup) {
        Ok(exit_code) => thread_exit(exit_code),
        Err(error) => {
            log_fmt(format_args!("stopped: {error}"));
            thread_exit(error.exit_code())
        }
    }
}

fn run(startup: Handle) -> Result<u64> {
    let ReceivedStartup { log, resources } =
        InitStartup::receive(OwnedHandle::from_handle(startup))?;
    install_log(log.into_raw(), LOG_TAG);
    let InitStartup {
        control,
        service,
        image,
        root,
        mounts,
        system,
    } = resources;
    // Экспорта у ступени 0 нет, и серверная сторона service-порта живёт до конца процесса
    // без обслуживания.
    let _service = service;

    let region = MemoryRegion::from_handle(image);
    let mapping = region.map_full_read_only()?;
    let table = boot_table(mapping.as_bytes())?;
    let admin =
        namespace_admin::Client::new(PortTransport::client(&mounts)).with_wait(CALL_TIMEOUT);
    let supervisor =
        supervisor::Client::new(PortTransport::client(&system)).with_wait(CALL_TIMEOUT);

    // Монтирование забирает сессию слота, а записи ступени 1 читаются после него: окно корня
    // даёт вторую сессию той же файловой системы.
    let reading = Reading::derive(&root)?;
    graft_root(&admin, table, root)?;
    log_line("boot root mounted");

    // Каталоги компонентов ступени 1 разрешает супервизор через маршрутизатор, и её подъём
    // идёт после корня.
    raise_stage_one(&reading, &supervisor)?;
    graft_raised(&admin, table, &supervisor)?;
    log_line("boot tree mounted");

    // Компонент, вышедший с нулём, композиция поднимает заново по политике плана, а повторное
    // монтирование его префиксов отвечает `AlreadyExists`.
    Ok(park(&PortTransport::server(control.borrow())))
}

fn boot_table(image: &[u8]) -> Result<MountTable<'_>> {
    let record = decode(image)
        .ok()
        .and_then(|entries| entries.entry(BOOT_MOUNT_TABLE_PATH))
        .ok_or(Error::MountTable)?;
    decode_mount_table(record.data()).map_err(|error| {
        log_fmt(format_args!("boot mount table rejected: {error:?}"));
        Error::MountTable
    })
}

fn graft_root(
    admin: &namespace_admin::Client<PortTransport>,
    table: MountTable<'_>,
    root: OwnedHandle,
) -> Result<()> {
    // Сминченная сессия не несёт `DUPLICATE`, и статически связанный корень прививается ровно
    // к одному префиксу.
    let mut root = Some(root);
    for mount in table {
        if mount.instance != ROOT_FILE_SYSTEM {
            continue;
        }
        graft(admin, mount.path, root.take().ok_or(Error::MountTable)?)?;
    }
    if root.is_some() {
        log_fmt(format_args!("boot mount table names no {ROOT_FILE_SYSTEM}"));
        return Err(Error::MountTable);
    }
    Ok(())
}

fn graft_raised(
    admin: &namespace_admin::Client<PortTransport>,
    table: MountTable<'_>,
    supervisor: &supervisor::Client<PortTransport>,
) -> Result<()> {
    for mount in table {
        if mount.instance == ROOT_FILE_SYSTEM {
            continue;
        }
        let instance = Str::<INSTANCE_NAME_MAX>::new(mount.instance).ok_or(Error::MountTable)?;
        let session = accepted("stage 1 session", supervisor.session(instance))?;
        graft(admin, mount.path, OwnedHandle::adopt(session))?;
    }
    Ok(())
}

fn graft(
    admin: &namespace_admin::Client<PortTransport>,
    path: &str,
    session: OwnedHandle,
) -> Result<()> {
    let prefix = Str::<PATH_MAX>::new(path).ok_or(Error::MountTable)?;
    admin.mount(prefix, session)?.map_err(|error| {
        log_fmt(format_args!("mount of {path} refused: {error:?}"));
        Error::Mount
    })?;
    Ok(())
}

fn raise_stage_one(
    reading: &Reading,
    supervisor: &supervisor::Client<PortTransport>,
) -> Result<()> {
    let records = reading.stage_one_records()?;
    if records.is_empty() {
        return Ok(());
    }
    accepted("stage 1 transaction", supervisor.begin())?;
    for record in &records {
        // Запись плана едет супервизору ровно такой, какой её закодировала сборка: путь
        // программы компонента `init` берёт из неё же.
        let program = reading.program_region(executable(record)?)?;
        let bytes = Bytes::<PLAN_RECORD_MAX>::new(record).ok_or(Error::StageOne)?;
        accepted(
            "stage 1 component",
            supervisor.define(bytes, program.into_handle()),
        )?;
    }
    match supervisor.commit()? {
        Ok(_) => Ok(()),
        // Набор ступени 1 принадлежит композиции, и свежая incarnation застаёт его поднятым.
        Err(ExtendError::Duplicate) => {
            log_line("stage 1 already raised");
            Ok(())
        }
        Err(error) => {
            log_fmt(format_args!("stage 1 commit refused: {error:?}"));
            Err(Error::StageOne)
        }
    }
}

fn executable(record: &[u8]) -> Result<&str> {
    decode_plan(record)
        .ok()
        .and_then(|plan| plan.component(0))
        .map(|component| component.executable())
        .ok_or(Error::StageOne)
}

impl Reading {
    fn derive(root: &OwnedHandle) -> Result<Self> {
        let files = file_system::Client::new(PortTransport::client(root)).with_wait(CALL_TIMEOUT);
        let window = Str::<PATH_MAX>::new(Path::ROOT.as_str()).ok_or(Error::StageOne)?;
        let session = OwnedHandle::adopt(answered("root window", files.open_root(window))?);
        let files =
            file_system::Client::new(PortTransport::client(&session)).with_wait(CALL_TIMEOUT);
        let region = MemoryRegion::adopt(answered(
            "stage 1 data buffer",
            files.acquire_buffer(BUFFER_SIZE),
        )?);
        let buffer = region.map_full(UserMemFlags::ReadWrite)?;
        Ok(Self {
            files,
            buffer,
            _region: region,
            _session: session,
        })
    }

    fn stage_one_records(&self) -> Result<Vec<Vec<u8>>> {
        // Образ без каталога ступени 1 несёт одну ступень 0.
        let Some(directory) = self.open_stage_one()? else {
            return Ok(Vec::new());
        };
        let names = self.list(directory.fd)?;
        self.close(directory.fd)?;
        names
            .iter()
            .map(|name| self.record(&format!("{BOOT_STAGE_ONE_DIRECTORY}/{name}")))
            .collect()
    }

    fn program_region(&self, executable: &str) -> Result<MemoryRegion> {
        let path = format!("{executable}/{COMPONENT_PROGRAM_NAME}");
        let opened = self.node(&path, OpenFlags::NONE)?;
        if opened.kind != NodeKind::File {
            log_fmt(format_args!("stage 1 program {path} is not a file"));
            return Err(Error::StageOne);
        }
        let region = answered("stage 1 program", self.files.backing_memory(opened.fd))?;
        self.close(opened.fd)?;
        Ok(MemoryRegion::adopt(region))
    }

    fn open_stage_one(&self) -> Result<Option<Opened>> {
        let path = Str::<PATH_MAX>::new(BOOT_STAGE_ONE_DIRECTORY).ok_or(Error::StageOne)?;
        match self
            .files
            .open(path, AccessMode::Read, OpenFlags::DIRECTORY)?
        {
            Ok(opened) => Ok(Some(opened)),
            Err(FileSystemError::NotFound) => Ok(None),
            Err(error) => {
                log_fmt(format_args!("stage 1 directory refused: {error:?}"));
                Err(Error::StageOne)
            }
        }
    }

    fn list(&self, fd: NodeDescriptor) -> Result<Vec<String>> {
        let mut names = Vec::new();
        let mut page = vec![0; BUFFER_SIZE as usize];
        let mut from = DirCursor::START;
        loop {
            let listed = answered("stage 1 listing", self.files.list(fd, from))?;
            self.buffer.copy_out(&mut page)?;
            collect_names(&page, listed.count, &mut names)?;
            if listed.end {
                return Ok(names);
            }
            // Перечисление обязано продвинуться: иначе записи не помещаются в буфер и повтор
            // вызова ничего не изменит.
            if listed.next.raw() <= from.raw() {
                log_line("stage 1 listing does not advance");
                return Err(Error::StageOne);
            }
            from = listed.next;
        }
    }

    fn record(&self, path: &str) -> Result<Vec<u8>> {
        let opened = self.node(path, OpenFlags::NONE)?;
        let len = u32::try_from(opened.size).map_err(|_| Error::StageOne)?;
        let count = answered("stage 1 record", self.files.read(opened.fd, 0, len))?;
        self.close(opened.fd)?;
        let mut bytes = vec![0; count as usize];
        self.buffer.copy_out(&mut bytes)?;
        Ok(bytes)
    }

    fn node(&self, path: &str, flags: OpenFlags) -> Result<Opened> {
        let name = Str::<PATH_MAX>::new(path).ok_or(Error::StageOne)?;
        self.files
            .open(name, AccessMode::Read, flags)?
            .map_err(|error| {
                log_fmt(format_args!("stage 1 record {path} refused: {error:?}"));
                Error::StageOne
            })
    }

    fn close(&self, fd: NodeDescriptor) -> Result<()> {
        answered("stage 1 descriptor", self.files.close(fd))?;
        Ok(())
    }
}

fn collect_names(page: &[u8], count: u32, names: &mut Vec<String>) -> Result<()> {
    let mut offset = 0;
    for _ in 0..count {
        let decoded =
            DirEntry::decode(page.get(offset..).ok_or(Error::StageOne)?).map_err(Error::Ipc)?;
        if decoded.entry.kind == NodeKind::File {
            names.push(decoded.entry.name.to_string());
        }
        offset += decoded.len;
    }
    Ok(())
}

fn answered<T>(
    operation: &str,
    answered: core::result::Result<core::result::Result<T, FileSystemError>, IpcError>,
) -> Result<T> {
    answered?.map_err(|error| {
        log_fmt(format_args!("{operation} refused: {error:?}"));
        Error::StageOne
    })
}

fn accepted<T>(
    operation: &str,
    answered: core::result::Result<core::result::Result<T, ExtendError>, IpcError>,
) -> Result<T> {
    answered?.map_err(|error| {
        log_fmt(format_args!("{operation} refused: {error:?}"));
        Error::StageOne
    })
}

fn park(control: &PortTransport) -> u64 {
    loop {
        match control.wait_readable(Timeout::INFINITE) {
            // Протокола на control-порту нет: пришедший кадр уходит без ответа, а закрытие
            // reply-объекта будит вызывателя.
            Ok(()) => control.cancel_reply(),
            // Уход значка поднявшего компонент supervisor снимает процесс.
            Err(IpcError::SenderGone) if supervisor_gone(control.peer_badge()) => {
                log_line("supervisor gone");
                return SUPERVISOR_LOST_EXIT_CODE;
            }
            // Покадровое исчерпание и уход чужого значка парковку не рвут.
            Err(IpcError::SenderGone | IpcError::ResourceExhausted | IpcError::Timeout) => {}
            Err(_) => {
                log_line("control port failed");
                return CONTROL_PORT_FAILED_EXIT_CODE;
            }
        }
    }
}

impl Error {
    const fn exit_code(&self) -> u64 {
        match self {
            Self::Runtime(_) => 2,
            Self::Ipc(_) => 3,
            Self::Start => 6,
            Self::MountTable => 10,
            Self::Mount => 11,
            Self::StageOne => 12,
        }
    }
}

impl Display for Error {
    fn fmt(&self, formatter: &mut Formatter<'_>) -> FmtResult {
        match self {
            Self::Runtime(error) => write!(formatter, "runtime error {error:?}"),
            Self::Ipc(error) => write!(formatter, "ipc error {error:?}"),
            Self::Start => formatter.write_str("no start frame"),
            Self::MountTable => formatter.write_str("boot mount table unavailable"),
            Self::Mount => formatter.write_str("boot tree not mounted"),
            Self::StageOne => formatter.write_str("stage 1 not raised"),
        }
    }
}

impl From<kernel_objects::Error> for Error {
    fn from(error: kernel_objects::Error) -> Self {
        Self::Runtime(error)
    }
}

impl From<IpcError> for Error {
    fn from(error: IpcError) -> Self {
        Self::Ipc(error)
    }
}

impl From<StartupError> for Error {
    fn from(error: StartupError) -> Self {
        match error {
            StartupError::Transport(error) => Self::Ipc(error),
            _ => Self::Start,
        }
    }
}

#[panic_handler]
fn panic(info: &PanicInfo<'_>) -> ! {
    log_fmt(format_args!("panic: {info}"));
    thread_exit(1)
}
