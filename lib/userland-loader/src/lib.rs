//! Загрузка программы из её [`EntryView`] в новый процесс из userspace.
//!
//! [`load_entry`] минтит свежие регионы под сегменты, копирует в них байты,
//! собирает дескриптор образа и стартует процесс через `ProcessCreate`/
//! `ProcessLoadImage`/`ProcessStart`. Ошибка после `ProcessCreate` завершает
//! пустой процесс служебным кодом и конечным ожиданием. [`launch_entry`]
//! добавляет стандартный startup-порт и отправляет через него стартовый кадр.
//! Источник байт абстрагирован за `EntryView`.

#![no_std]
#![allow(unsafe_code)]

extern crate alloc;

use alloc::vec::Vec;
use core::num::NonZeroU64;

use kernel_objects::{
    Error, Mapping, MemoryRegion, OwnedHandle, Port, Process, Resource, Result, Signal, ThreadEntry,
};
use runtime::{MemoryAccess, Priority, Timeout, UserMemFlags};
use syscall::{
    SEGMENT_ABI_VERSION, SIGNALED, SyscallError, USER_VA_END, UserImageDescAbi, UserSegmentAbi,
    encode_image_desc, encode_segment,
};
use userland::{
    EntryView, SegmentPermissions, SegmentView, USERLAND_PAGE_SIZE, entry_name, user_vm_window,
};

const STARTUP_FAILURE_EXIT_CODE: u32 = 1;
const STARTUP_CLEANUP_TIMEOUT: Timeout = Timeout::from_secs(5);
const CREATED_PROCESS_REAP_TURN: Timeout = Timeout::from_millis(1);

struct PendingLaunch {
    process: Option<Process>,
    startup: Option<Port>,
}

struct PendingProcess {
    process: Option<Process>,
}

/// Процесс с завершённой startup-транзакцией и сохранённым клиентским портом.
#[derive(Debug)]
pub struct InitializedProcess {
    /// Запущенный процесс.
    pub process: Process,
    /// Клиентская сторона startup-протокола.
    pub startup: Port,
}

impl PendingLaunch {
    fn finish<E>(
        mut self,
        send: impl FnOnce(&Port) -> core::result::Result<(), E>,
    ) -> core::result::Result<Process, E>
    where
        E: From<Error>,
    {
        if let Err(error) = send(self.startup()) {
            return match self.rollback() {
                Ok(()) => Err(error),
                Err(cleanup) => Err(E::from(cleanup)),
            };
        }
        let process = self.take_process();
        self.startup = None;
        Ok(process)
    }

    fn retain<E>(
        mut self,
        send: impl FnOnce(&Port) -> core::result::Result<(), E>,
    ) -> core::result::Result<InitializedProcess, E>
    where
        E: From<Error>,
    {
        if let Err(error) = send(self.startup()) {
            return match self.rollback() {
                Ok(()) => Err(error),
                Err(cleanup) => Err(E::from(cleanup)),
            };
        }
        Ok(InitializedProcess {
            process: self.take_process(),
            startup: self.startup.take().expect("startup port is present"),
        })
    }

    fn startup(&self) -> &Port {
        self.startup.as_ref().expect("startup port is present")
    }

    fn take_process(&mut self) -> Process {
        self.process.take().expect("launched process is present")
    }

    fn rollback(mut self) -> Result<()> {
        self.startup = None;
        let Some(process) = self.process.as_ref() else {
            return Ok(());
        };
        terminate_and_join(process)?;
        self.process = None;
        Ok(())
    }
}

impl Drop for PendingLaunch {
    fn drop(&mut self) {
        if let Some(process) = self.process.as_ref() {
            let _ = process.terminate(STARTUP_FAILURE_EXIT_CODE);
        }
    }
}

impl PendingProcess {
    fn new(process: Process) -> Self {
        Self {
            process: Some(process),
        }
    }

    fn process(&self) -> &Process {
        self.process.as_ref().expect("created process is present")
    }

    fn finish(mut self) -> Process {
        self.process.take().expect("created process is present")
    }

    fn fail<T>(self, error: Error) -> Result<T> {
        match self.rollback() {
            Ok(()) => Err(error),
            Err(cleanup) => Err(cleanup),
        }
    }

    fn rollback(mut self) -> Result<()> {
        let Some(process) = self.process.as_ref() else {
            return Ok(());
        };
        terminate_and_join(process)?;
        confirm_created_process_reaped()?;
        self.process = None;
        Ok(())
    }
}

impl Drop for PendingProcess {
    fn drop(&mut self) {
        if let Some(process) = self.process.as_ref() {
            let _ = process.terminate(STARTUP_FAILURE_EXIT_CODE);
        }
    }
}

fn terminate_and_join(process: &Process) -> Result<()> {
    process.terminate(STARTUP_FAILURE_EXIT_CODE)?;
    process.join(STARTUP_CLEANUP_TIMEOUT)
}

fn confirm_created_process_reaped() -> Result<()> {
    let timer = Signal::create()?;
    match timer.wait(SIGNALED, CREATED_PROCESS_REAP_TURN) {
        Err(Error::Syscall(SyscallError::Timeout)) => Ok(()),
        Err(error) => Err(error),
        Ok(_) => Err(Error::Syscall(SyscallError::Canceled)),
    }
}

/// Загружает `entry`, отправляет startup-кадр через одноразовый порт и
/// возвращает процесс.
///
/// Ошибка callback завершает потомка синхронным `terminate`/`join` с конечным
/// сроком. Отказ этого пути заменяет исходную ошибку callback.
pub fn launch_entry<'a, V: EntryView<'a>, E: From<Error>>(
    entry: &V,
    name: Option<&str>,
    resource: &Resource,
    startup: impl FnOnce(&Port) -> core::result::Result<(), E>,
) -> core::result::Result<Process, E> {
    prepare_launch(entry, name, resource, None)
        .map_err(E::from)?
        .finish(startup)
}

/// Загружает `entry`, отправляет startup-кадр и возвращает процесс с
/// сохранённой клиентской стороной порта под значком `lease`.
///
/// Значок делает исчезновение вызывающего наблюдаемым: потомок, оставшийся без
/// держателей `lease`, получает `SenderGone` очередным приёмом на этом порту.
/// Ошибка callback проходит тот же cleanup, что и у [`launch_entry`].
pub fn launch_entry_retained<'a, V: EntryView<'a>, E: From<Error>>(
    entry: &V,
    name: Option<&str>,
    resource: &Resource,
    lease: NonZeroU64,
    startup: impl FnOnce(&Port) -> core::result::Result<(), E>,
) -> core::result::Result<InitializedProcess, E> {
    prepare_launch(entry, name, resource, Some(lease))
        .map_err(E::from)?
        .retain(startup)
}

fn prepare_launch<'a, V: EntryView<'a>>(
    entry: &V,
    name: Option<&str>,
    resource: &Resource,
    badge: Option<NonZeroU64>,
) -> Result<PendingLaunch> {
    let startup = Port::create_service(badge)?;
    let process = load_entry(entry, name, resource, startup.server)?;
    Ok(PendingLaunch {
        process: Some(process),
        startup: Some(startup.client),
    })
}

/// Загружает `entry` в новый процесс и стартует его первый поток.
///
/// Сегменты копируются в свежие приватные регионы. `resource` оплачивает их и
/// становится метеринг-ресурсом потомка. `name` называет процесс, а без имени
/// он зовётся по записи программы. `initial` — начальная capability. `Ok`
/// означает, что хэндл передан потомку.
pub fn load_entry<'a, E: EntryView<'a>>(
    entry: &E,
    name: Option<&str>,
    resource: &Resource,
    initial: OwnedHandle,
) -> Result<Process> {
    // Регионы, маппинги и буфер сегментов держим живыми до возврата из
    // `start`: регионы должны существовать до установки в child-AS.
    let mut regions: Vec<MemoryRegion> = Vec::new();
    let mut mappings: Vec<Mapping> = Vec::new();
    let mut segment_abi: Vec<u8> = Vec::new();
    let mut segment_count: u32 = 0;

    for seg in entry.segments() {
        let mapped = round_up_to_page(seg.mem_size());
        let region = resource.create_virtual(mapped, MemoryAccess::RWX)?;
        let mapping = region.map(mapped, UserMemFlags::ReadWrite)?;

        let bytes = seg.bytes();
        let dst = mapping.va() as *mut u8;
        // SAFETY: map выдал RW-маппинг размером `mapped` >= `mem_size` >=
        // `bytes.len()` (инвариант валидного образа); пишем в его границах.
        // Хвост за `bytes` остаётся нулевым (регион зануляется при первом map).
        unsafe {
            core::ptr::copy_nonoverlapping(bytes.as_ptr(), dst, bytes.len());
        }

        let abi = UserSegmentAbi {
            region_handle: region.handle().as_raw().raw(),
            flags: permissions_to_flags(seg.permissions()).raw() as u32,
            va_base: seg.va_base(),
            mapped_size: mapped,
            reserved: 0,
        };
        segment_abi.extend_from_slice(&encode_segment(&abi));

        regions.push(region);
        mappings.push(mapping);
        segment_count += 1;
    }

    // Стек - на потолке user-AS; окно кучи (user_vm) - промежуток между концом
    // сегментов и низом стека, выведенный из самого образа.
    let stack_top = USER_VA_END;
    let stack_size = round_up_to_page(entry.stack_size());
    let window = user_vm_window(
        entry.segments().map(|seg| (seg.va_base(), seg.mem_size())),
        stack_top,
        stack_size,
    )
    .ok_or(Error::Syscall(SyscallError::InvalidArgument))?;

    // Стек - обычный сегмент: ядро о нём отдельно не знает, а его страницы
    // оплачивает тот же `resource`, что и остальной образ.
    let stack = resource.create_virtual(stack_size, MemoryAccess::RW)?;
    let stack_abi = UserSegmentAbi {
        region_handle: stack.handle().as_raw().raw(),
        flags: UserMemFlags::ReadWrite.raw() as u32,
        va_base: stack_top - stack_size,
        mapped_size: stack_size,
        reserved: 0,
    };
    segment_abi.extend_from_slice(&encode_segment(&stack_abi));
    segment_count += 1;
    regions.push(stack);

    let desc = UserImageDescAbi {
        version: SEGMENT_ABI_VERSION,
        segment_count,
        segments_va: segment_abi.as_ptr() as u64,
        entry_va: entry.entry_va(),
        user_vm_base: window.start,
        user_vm_size: window.end - window.start,
    };

    let name = name.unwrap_or_else(|| entry_name(entry.path()));
    let pending = PendingProcess::new(Process::create(name)?);
    if let Err(error) = pending.process().load_image(&encode_image_desc(&desc)) {
        return pending.fail(error);
    }
    let thread = match pending.process().start(
        ThreadEntry {
            entry_pc: entry.entry_va(),
            user_sp: stack_top,
            arg: 0,
            priority: Priority::new(1),
        },
        initial,
        Some(resource),
    ) {
        Ok(thread) => thread,
        Err(error) => return pending.fail(error),
    };

    // Регионы и маппинги прочитаны ядром при load_image/start; thread-handle
    // здесь не нужен (процесс ждут по его handle).
    drop(thread);
    drop(mappings);
    drop(regions);
    Ok(pending.finish())
}

fn permissions_to_flags(permissions: SegmentPermissions) -> UserMemFlags {
    match permissions {
        SegmentPermissions::ReadWrite => UserMemFlags::ReadWrite,
        SegmentPermissions::ReadOnly => UserMemFlags::ReadOnly,
        SegmentPermissions::ReadExecute => UserMemFlags::ReadExecute,
    }
}

fn round_up_to_page(value: u64) -> u64 {
    value.div_ceil(USERLAND_PAGE_SIZE) * USERLAND_PAGE_SIZE
}
