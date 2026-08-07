//! Таблица монтирования и её обслуживание с двух портов: сервисная сторона разбирает пути в
//! сессии, admin-сторона монтирует, снимает и наблюдает файловые системы. Данные через
//! маршрутизатор не идут - он только поднимает файловые системы и устанавливает клиентам сессии
//! с ними.
//!
//! Таблицу меняет один поток admin-порта: монтирование, снятие и смена ушедшего экземпляра идут
//! его оборотами, а сервисный поток таблицу только читает.

use alloc::{
    string::{String, ToString},
    vec::Vec,
};
use core::num::{NonZeroU32, NonZeroU64};

use filesystem_abi::{
    FileSystemAdminClient, FileSystemError, MountTable as BootMountTable, Mounted,
    NamespaceAdminService, NamespaceService, PATH_MAX, Path,
};
use filesystem_server::{settle, vend};
use ipc::wire::Str;
use runtime::{
    MemoryRegion, Mutex, OwnedHandle, Port, PortTransport, Process, Resource, Timeout, log_fmt,
    log_line,
};
use syscall::{Handle, Rights};
use userland_image::decode;
use userland_loader::launch_entry_retained;

use crate::error::{Error, Result as RouterResult};

const DONE: u32 = 0;

const MOUNT_BUDGET_PAGES: u64 = 512;
const UNMOUNT_TIMEOUT: Timeout = Timeout::from_secs(5);

const MOUNT_GENERATION_MAX: NonZeroU32 = NonZeroU32::new(3).expect("generation limit is non-zero");

const FIRST_GENERATION: NonZeroU32 = NonZeroU32::new(1).expect("first generation is non-zero");

const MOUNT_BUDGET_TIMEOUT: Timeout = Timeout::from_ns(500_000_000);

const BIND_TIMEOUT: Timeout = Timeout::from_secs(1);

const MOUNT_LEASE_BADGE: NonZeroU64 = NonZeroU64::new(1).expect("lease badge is non-zero");

#[derive(Clone)]
struct FileSystemProgram(String);

impl FileSystemProgram {
    fn new(path: &str) -> Self {
        Self(path.to_string())
    }

    fn as_str(&self) -> &str {
        &self.0
    }
}

struct Incarnation {
    admin: OwnedHandle,
    process: Process,
}

#[derive(Clone)]
struct MountRecord {
    prefix: String,
    program: FileSystemProgram,
    generation: NonZeroU32,
}

struct Mount {
    record: MountRecord,
    incarnation: Option<Incarnation>,
}

struct GoneMount {
    record: MountRecord,
    exit_code: Option<u32>,
}

pub(crate) struct MountTable {
    mounts: Vec<Mount>,
}

impl MountTable {
    pub(crate) const fn new() -> Self {
        Self { mounts: Vec::new() }
    }

    fn longest_prefix(&self, path: Path<'_>) -> Option<&Mount> {
        self.mounts
            .iter()
            .filter(|mount| {
                Path::new(&mount.record.prefix)
                    .is_ok_and(|prefix| path.strip_prefix(prefix).is_some())
            })
            .max_by_key(|mount| mount.record.prefix.len())
    }

    fn covers(&self, prefix: Path<'_>) -> bool {
        self.mounts
            .iter()
            .any(|mount| mount.record.prefix == prefix.as_str())
    }

    fn take_gone(&mut self) -> Option<GoneMount> {
        let mount = self.mounts.iter_mut().find(|mount| mount.terminated())?;
        let incarnation = mount.incarnation.take()?;
        Some(GoneMount {
            record: mount.record.clone(),
            exit_code: incarnation.process.exit_code().ok(),
        })
    }
}

impl Mount {
    fn terminated(&self) -> bool {
        self.incarnation
            .as_ref()
            .is_some_and(|incarnation| incarnation.process.join(Timeout::POLL).is_ok())
    }
}

struct MountTarget {
    admin: Handle,
    prefix_len: u32,
}

pub(crate) struct Resolving<'a> {
    table: &'a Mutex<MountTable>,
    vended: Option<OwnedHandle>,
}

impl<'a> Resolving<'a> {
    pub(crate) fn new(table: &'a Mutex<MountTable>) -> Self {
        Self {
            table,
            vended: None,
        }
    }

    pub(crate) fn end_frame(&mut self, delivered: bool) {
        settle(&mut self.vended, delivered);
    }

    fn target(&self, path: Path<'_>) -> Result<MountTarget, FileSystemError> {
        let table = self.table.lock();
        let mount = table
            .longest_prefix(path)
            .ok_or(FileSystemError::NotFound)?;
        // Монтирование между экземплярами накрывает путь, но сессий не выдаёт: NotFound остаётся
        // за путём, которого не накрывает ни одно монтирование.
        let incarnation = mount
            .incarnation
            .as_ref()
            .ok_or(FileSystemError::Unresponsive)?;
        Ok(MountTarget {
            admin: incarnation.admin.as_raw(),
            prefix_len: prefix_len(&mount.record.prefix),
        })
    }
}

impl NamespaceService for Resolving<'_> {
    fn resolve(&mut self, path: Str<'_, PATH_MAX>) -> Result<Mounted, FileSystemError> {
        let path = Path::new(path.as_str())?;
        let target = self.target(path)?;

        // Значок сессии минтит сама файловая система: политика допуска принадлежит ей, а
        // маршрутизатор только переносит выданную капу клиенту.
        let bound = FileSystemAdminClient::new(PortTransport::client(target.admin))
            .with_wait(BIND_TIMEOUT)
            .bind()
            .map_err(|error| {
                log_fmt(format_args!("bind refused: {error:?}"));
                FileSystemError::Unresponsive
            })??;
        Ok(Mounted {
            session: vend(&mut self.vended, OwnedHandle::adopt(bound)),
            prefix_len: target.prefix_len,
        })
    }
}

pub(crate) struct Administering<'a> {
    table: &'a Mutex<MountTable>,
    image_bytes: &'static [u8],
    image: MemoryRegion,
    log: OwnedHandle,
    resource: Resource,
}

impl<'a> Administering<'a> {
    pub(crate) fn new(
        table: &'a Mutex<MountTable>,
        image_bytes: &'static [u8],
        image: MemoryRegion,
        log: OwnedHandle,
        resource: Resource,
    ) -> Self {
        Self {
            table,
            image_bytes,
            image,
            log,
            resource,
        }
    }

    pub(crate) fn mount_boot_table(&self, table: BootMountTable<'_>) -> RouterResult<()> {
        // Отказ монтирования уносит процесс маршрутизатора, а с ним и уже поднятые
        // монтирования: наружу дерево уходит либо целиком, либо никак.
        for mount in table {
            let prefix = Path::new(mount.path).map_err(|_| Error::MountTable)?;
            self.mount_prefix(prefix, FileSystemProgram::new(mount.program))
                .map_err(|error| {
                    log_fmt(format_args!("boot mount {} refused: {error:?}", mount.path));
                    Error::MountTable
                })?;
        }
        Ok(())
    }

    fn mount_prefix(
        &self,
        prefix: Path<'_>,
        program: FileSystemProgram,
    ) -> Result<u32, FileSystemError> {
        // Гонки на занятости нет: таблицу меняет один admin-поток.
        if self.table.lock().covers(prefix) {
            return Err(FileSystemError::AlreadyExists);
        }
        // Подъём идёт вне лока: сервисный поток продолжает разбирать пути, пока файловая
        // система стартует.
        let budget = self
            .resource
            .split_when_available(MOUNT_BUDGET_PAGES, MOUNT_BUDGET_TIMEOUT)
            .map_err(|error| {
                log_fmt(format_args!("mount budget refused: {error:?}"));
                Error::Runtime(error).as_file_system_error()
            })?;
        let incarnation = self.found(&program, &budget).map_err(|error| {
            log_fmt(format_args!("mount refused: {error}"));
            error.as_file_system_error()
        })?;
        self.table.lock().mounts.push(Mount {
            record: MountRecord {
                prefix: prefix.as_str().to_string(),
                program,
                generation: FIRST_GENERATION,
            },
            incarnation: Some(incarnation),
        });
        log_line("mounted");
        Ok(DONE)
    }

    pub(crate) fn supervise(&self) {
        while let Some(gone) = self.gone_mount() {
            log_fmt(format_args!(
                "mount {} left after generation {}: exit code {:?}",
                gone.record.prefix, gone.record.generation, gone.exit_code
            ));
        }
        for record in self.mounts_without_incarnation() {
            if !self.replace(&record) {
                return;
            }
        }
    }

    fn replace(&self, record: &MountRecord) -> bool {
        let prefix = record.prefix.as_str();
        if record.generation >= MOUNT_GENERATION_MAX {
            log_fmt(format_args!("mount {prefix} reached its restart limit"));
            self.forget(prefix);
            return true;
        }
        let Ok(budget) = self.resource.split(MOUNT_BUDGET_PAGES) else {
            return false;
        };
        match self.found(&record.program, &budget) {
            Ok(incarnation) => self.attach(prefix, incarnation),
            Err(error) => {
                log_fmt(format_args!("mount {prefix} not restarted: {error}"));
                self.forget(prefix);
            }
        }
        true
    }

    fn gone_mount(&self) -> Option<GoneMount> {
        self.table.lock().take_gone()
    }

    fn mounts_without_incarnation(&self) -> Vec<MountRecord> {
        self.table
            .lock()
            .mounts
            .iter()
            .filter(|mount| mount.incarnation.is_none())
            .map(|mount| mount.record.clone())
            .collect()
    }

    fn found(&self, program: &FileSystemProgram, budget: &Resource) -> RouterResult<Incarnation> {
        let image = decode(self.image_bytes)?;
        let entry = image.entry(program.as_str()).ok_or(Error::NoProgram)?;
        let program = entry.program().map_err(|_| Error::NoProgram)?;

        let initialized = launch_entry_retained(&program, budget, MOUNT_LEASE_BADGE, |startup| {
            self.start_frame(startup)
        })?;
        Ok(Incarnation {
            admin: initialized.startup.into_handle(),
            process: initialized.process,
        })
    }

    fn attach(&self, prefix: &str, incarnation: Incarnation) {
        // Лок не переживает запись: журнал уходит кадром другому процессу, а таблица нужна
        // сервисному потоку.
        let generation = {
            let mut table = self.table.lock();
            let Some(mount) = table
                .mounts
                .iter_mut()
                .find(|mount| mount.record.prefix == prefix)
            else {
                return;
            };
            mount.record.generation = mount.record.generation.saturating_add(1);
            mount.incarnation = Some(incarnation);
            mount.record.generation
        };
        log_fmt(format_args!(
            "mount {prefix} restarted as generation {generation}"
        ));
    }

    fn forget(&self, prefix: &str) {
        self.table
            .lock()
            .mounts
            .retain(|mount| mount.record.prefix != prefix);
    }

    fn start_frame(&self, startup: &Port) -> RouterResult<()> {
        let log = self.log.duplicate(Rights::WRITE | Rights::TRANSFER, None)?;
        // Капа образа сохраняет `DUPLICATE` для минта окон узлов.
        let image = self
            .image
            .handle()
            .duplicate(Rights::READ | Rights::TRANSFER | Rights::DUPLICATE, None)?;
        FileSystemAdminClient::new(PortTransport::client(startup.handle())).start(log, image)?;
        Ok(())
    }
}

impl NamespaceAdminService for Administering<'_> {
    fn mount(
        &mut self,
        path: Str<'_, PATH_MAX>,
        program: Str<'_, PATH_MAX>,
    ) -> Result<u32, FileSystemError> {
        let prefix = Path::new(path.as_str())?;
        self.mount_prefix(prefix, FileSystemProgram::new(program.as_str()))
    }

    fn unmount(&mut self, path: Str<'_, PATH_MAX>) -> Result<u32, FileSystemError> {
        let prefix = Path::new(path.as_str())?;
        let mount = {
            let mut table = self.table.lock();
            let index = table
                .mounts
                .iter()
                .position(|mount| mount.record.prefix == prefix.as_str())
                .ok_or(FileSystemError::NotFound)?;
            table.mounts.remove(index)
        };
        // Снятое между экземплярами монтирование процесса не держит.
        let Some(incarnation) = mount.incarnation else {
            log_line("unmounted");
            return Ok(DONE);
        };
        // Снятие монтирования снимает и его процесс: выданные им сессии ядро отзывает каскадом
        // вместе с корневой капой его сервисного порта. Terminate идёт до освобождения
        // lease-капы: иначе процесс уходит кодом потери supervisor.
        incarnation.process.terminate(0).map_err(|error| {
            log_fmt(format_args!("unmount terminate refused: {error:?}"));
            Error::Runtime(error).as_file_system_error()
        })?;
        incarnation.process.join(UNMOUNT_TIMEOUT).map_err(|error| {
            log_fmt(format_args!("unmount join refused: {error:?}"));
            Error::Runtime(error).as_file_system_error()
        })?;
        log_line("unmounted");
        Ok(DONE)
    }
}

fn prefix_len(prefix: &str) -> u32 {
    if prefix == Path::ROOT.as_str() {
        return 0;
    }
    prefix.len() as u32
}
