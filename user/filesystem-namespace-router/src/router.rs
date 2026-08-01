//! Таблица монтирования и её обслуживание с двух портов: сервисная сторона разбирает пути в
//! сессии, admin-сторона монтирует и снимает файловые системы. Данные через маршрутизатор не
//! идут - он только поднимает файловые системы и устанавливает клиентам сессии с ними.

use alloc::{
    string::{String, ToString},
    vec::Vec,
};

use filesystem_abi::{
    FileSystemAdminClient, FileSystemError, Mounted, NamespaceAdminService, NamespaceService,
    PATH_MAX, Path,
};
use filesystem_server::{settle, vend};
use ipc::wire::{Cap, IpcError, Str};
use runtime::{
    MemoryRegion, Mutex, OwnedHandle, Port, PortTransport, Process, Resource, log_fmt, log_line,
};
use syscall::Rights;
use userland_image::decode;
use userland_loader::load_entry;

use crate::error::{Error, Result as RouterResult};

// Результат операции, у которой нет собственного возврата.
const DONE: u32 = 0;

// Бюджет страниц, выдаваемый процессу файловой системы: его образ, стек, куча и буферы данных
// сессий укладываются в него, а исчерпание не задевает ни маршрутизатор, ни соседей.
const MOUNT_BUDGET_PAGES: u64 = 512;

// Срок ответа файловой системы на bind. Монтирование, не ответившее в срок, отпускает поток
// диспетчеризации, а клиенту уходит Unresponsive.
const BIND_TIMEOUT_NS: u64 = 1_000_000_000;

// Монтирование: префикс дерева имён и процесс, который его обслуживает.
struct Mount {
    /// Префикс в форме, проверенной [`Path`]. Строка своя: пути приезжают кадрами и живут только
    /// до конца обработчика.
    prefix: String,
    /// Клиентская сторона admin-порта файловой системы: по ней идёт `bind`.
    admin: OwnedHandle,
    /// Процесс файловой системы: поднявший его маршрутизатор его же и снимает.
    process: Process,
}

/// Таблица монтирования, разделённая потоками сервисного и admin-порта.
pub(crate) struct MountTable {
    mounts: Vec<Mount>,
}

impl MountTable {
    pub(crate) const fn new() -> Self {
        Self { mounts: Vec::new() }
    }

    // Монтирование, чей префикс совпал с путём и оказался самым длинным.
    fn longest_prefix(&self, path: Path<'_>) -> Option<&Mount> {
        self.mounts
            .iter()
            .filter(|mount| {
                Path::new(&mount.prefix).is_ok_and(|prefix| path.strip_prefix(prefix).is_some())
            })
            .max_by_key(|mount| mount.prefix.len())
    }

    fn covers(&self, prefix: Path<'_>) -> bool {
        self.mounts
            .iter()
            .any(|mount| mount.prefix == prefix.as_str())
    }
}

/// Сервисная сторона маршрутизатора: разбирает пути клиентов в сессии файловых систем.
pub(crate) struct Resolving<'a> {
    table: &'a Mutex<MountTable>,
    /// Capability, уложенная в кадр ответа: пока ответ не доставлен, она принадлежит
    /// маршрутизатору.
    vended: Option<OwnedHandle>,
}

impl<'a> Resolving<'a> {
    pub(crate) fn new(table: &'a Mutex<MountTable>) -> Self {
        Self {
            table,
            vended: None,
        }
    }

    /// Закрывает кадр: выданная capability остаётся за клиентом только при доставленном ответе -
    /// иначе ядро её не переносило и закрыть её некому.
    pub(crate) fn end_frame(&mut self, delivered: bool) {
        settle(&mut self.vended, delivered);
    }
}

impl NamespaceService for Resolving<'_> {
    fn resolve(&mut self, path: Str<'_, PATH_MAX>) -> Result<Mounted, FileSystemError> {
        let path = Path::new(path.as_str())?;
        let table = self.table.lock();
        let mount = table
            .longest_prefix(path)
            .ok_or(FileSystemError::NotFound)?;

        // Значок сессии минтит сама файловая система: политика допуска принадлежит ей, а
        // маршрутизатор только переносит выданную капу клиенту.
        let bound = FileSystemAdminClient::new(PortTransport::client(&mount.admin))
            .with_wait_ns(BIND_TIMEOUT_NS)
            .bind()
            .map_err(|error| {
                log_fmt(format_args!("bind refused: {error:?}"));
                // Монтирование, не ответившее в срок, клиент отличает от отсутствующего: дерево
                // имён путь накрывает, но файловая система за ним не отвечает.
                match error {
                    IpcError::Timeout => FileSystemError::Unresponsive,
                    _ => FileSystemError::NotFound,
                }
            })??;
        let prefix_len = prefix_len(&mount.prefix);
        Ok(Mounted {
            session: vend(&mut self.vended, OwnedHandle::adopt(bound)),
            prefix_len,
        })
    }
}

/// Admin-сторона маршрутизатора: держит ресурсы подъёма файловых систем и меняет таблицу
/// монтирования.
pub(crate) struct Administering<'a> {
    table: &'a Mutex<MountTable>,
    /// Байты userland-образа: из них берутся программы файловых систем.
    image_bytes: &'static [u8],
    /// Регион образа: его копия уходит каждой поднятой файловой системе.
    image: MemoryRegion,
    /// Канал логирования: его копия уходит каждой поднятой файловой системе.
    log: OwnedHandle,
    /// Metering resource маршрутизатора: из него нарезаются бюджеты монтирований.
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

    // Поднимает файловую систему из записи образа: заводит ей admin-порт, отрезает бюджет и
    // шлёт стартовый кадр.
    fn found(&self, program: &str) -> RouterResult<(Process, OwnedHandle)> {
        let image = decode(self.image_bytes)?;
        let entry = image.entry(program).ok_or(Error::NoProgram)?;
        let program = entry.program().map_err(|_| Error::NoProgram)?;

        // Серверная сторона порта уезжает файловой системе целиком: получателем на нём числится
        // она одна, и её смерть маршрутизатору видна.
        let admin = Port::create_service(0)?;
        let budget = self.resource.split(MOUNT_BUDGET_PAGES)?;
        let process = load_entry(&program, &budget, admin.server)?;

        let log = self.log.duplicate(Rights::WRITE | Rights::TRANSFER, 0)?;
        // Право минта уходит вместе с капой: файловая система поверх образа режет из него окна
        // узлов, а деривация региона требует `DUPLICATE`.
        let image = self
            .image
            .handle()
            .duplicate(Rights::READ | Rights::TRANSFER | Rights::DUPLICATE, 0)?;
        FileSystemAdminClient::new(PortTransport::client(admin.client.handle()))
            .start(log, image)?;
        Ok((process, admin.client.into_handle()))
    }
}

impl NamespaceAdminService for Administering<'_> {
    /// Стартовый кадр маршрутизатор принимает один раз, до подъёма. Повторный несёт capability,
    /// которых он не ждёт: они закрываются вместе с кадром.
    fn start(&mut self, log: Cap, image: Cap, service: Cap) {
        drop(OwnedHandle::adopt(log));
        drop(OwnedHandle::adopt(image));
        drop(OwnedHandle::adopt(service));
        log_line("repeated start frame ignored");
    }

    fn mount(
        &mut self,
        path: Str<'_, PATH_MAX>,
        program: Str<'_, PATH_MAX>,
    ) -> Result<u32, FileSystemError> {
        let prefix = Path::new(path.as_str())?;
        // Гонки на занятости нет: таблицу меняет один admin-поток.
        if self.table.lock().covers(prefix) {
            return Err(FileSystemError::AlreadyExists);
        }
        // Подъём идёт вне лока: сервисный поток продолжает разбирать пути, пока файловая
        // система стартует.
        let (process, admin) = self.found(program.as_str()).map_err(|error| {
            log_fmt(format_args!("mount refused: {error}"));
            error.as_file_system_error()
        })?;
        self.table.lock().mounts.push(Mount {
            prefix: prefix.as_str().to_string(),
            admin,
            process,
        });
        log_line("mounted");
        Ok(DONE)
    }

    fn unmount(&mut self, path: Str<'_, PATH_MAX>) -> Result<u32, FileSystemError> {
        let prefix = Path::new(path.as_str())?;
        let mount = {
            let mut table = self.table.lock();
            let index = table
                .mounts
                .iter()
                .position(|mount| mount.prefix == prefix.as_str())
                .ok_or(FileSystemError::NotFound)?;
            table.mounts.remove(index)
        };
        // Снятие монтирования снимает и его процесс: выданные им сессии ядро отзывает каскадом
        // вместе с корневой капой его сервисного порта.
        let _ = mount.process.terminate(0);
        log_line("unmounted");
        Ok(DONE)
    }
}

// Сколько байт пути накрывает префикс. Корень не накрывает ничего: путь под ним идёт файловой
// системе как есть.
fn prefix_len(prefix: &str) -> u32 {
    if prefix == Path::ROOT.as_str() {
        return 0;
    }
    prefix.len() as u32
}
