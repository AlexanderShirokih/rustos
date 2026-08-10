//! Программа-фикстура падающего монтирования: изменяемое дерево, которое обращается по
//! невалидному адресу на переносе имени. Ядро завершает виновный процесс целиком, и
//! rootkeeper наблюдает отказ компонента файловой системы.
//!
//! Перенос выбран за то, что сервер зовёт его только по запросу клиента: `unlink` он вызывает
//! ещё и откатом неудавшегося `open` с `CREATE`.

#![no_std]
#![no_main]
#![allow(unsafe_code)]

extern crate alloc;

use core::panic::PanicInfo;

use component_abi::component_manifest;
use filesystem::{FileSystemBackend, NodeId, RamFileSystem, Window};
use filesystem_abi::{DirCursor, FileSystemError, Listed, NodeInfo, NodeKind, Path};
use filesystem_server::{Error, Result as ServerResult, serve};
use runtime::{OwnedHandle, PortTransport, log_fmt, thread_exit};
use syscall::{Handle, USER_VA_END};

const LOG_TAG: &str = "crashfs";

component_manifest! {
    struct CrashFileSystemStartup {
        slots: {},
    }
}

struct CrashingFileSystem {
    inner: RamFileSystem,
}

impl FileSystemBackend for CrashingFileSystem {
    fn is_mutable(&self) -> bool {
        self.inner.is_mutable()
    }

    fn lookup(&self, path: Path<'_>) -> Result<NodeId, FileSystemError> {
        self.inner.lookup(path)
    }

    fn create(&mut self, path: Path<'_>, kind: NodeKind) -> Result<NodeId, FileSystemError> {
        self.inner.create(path, kind)
    }

    fn unlink(&mut self, path: Path<'_>) -> Result<(), FileSystemError> {
        self.inner.unlink(path)
    }

    fn rename(&mut self, _from: Path<'_>, _to: Path<'_>) -> Result<(), FileSystemError> {
        kill_process_by_fault()
    }

    fn metadata(&self, node: NodeId) -> Result<NodeInfo, FileSystemError> {
        self.inner.metadata(node)
    }

    fn read_at(&self, node: NodeId, offset: u64, out: &mut [u8]) -> Result<usize, FileSystemError> {
        self.inner.read_at(node, offset, out)
    }

    fn write_at(
        &mut self,
        node: NodeId,
        offset: u64,
        data: &[u8],
    ) -> Result<usize, FileSystemError> {
        self.inner.write_at(node, offset, data)
    }

    fn append(&mut self, node: NodeId, data: &[u8]) -> Result<u64, FileSystemError> {
        self.inner.append(node, data)
    }

    fn set_size(&mut self, node: NodeId, size: u64) -> Result<(), FileSystemError> {
        self.inner.set_size(node, size)
    }

    fn list(
        &self,
        node: NodeId,
        from: DirCursor,
        out: &mut [u8],
    ) -> Result<Listed, FileSystemError> {
        self.inner.list(node, from, out)
    }

    fn backing(&self, node: NodeId) -> Result<Window, FileSystemError> {
        self.inner.backing(node)
    }
}

fn kill_process_by_fault() -> ! {
    let unmapped = USER_VA_END as *const u64;
    // SAFETY: адрес намеренно недоступен из EL0 - фикстура проверяет завершение процесса ядром.
    unsafe {
        unmapped.read_volatile();
    }
    thread_exit(1)
}

/// `startup` приходит в x0 как HandleId одноразового startup-порта.
#[unsafe(no_mangle)]
pub extern "C" fn _start(startup: Handle) -> ! {
    match run(startup) {
        Ok(()) => thread_exit(0),
        Err(error) => {
            log_fmt(format_args!("stopped: {error}"));
            thread_exit(1)
        }
    }
}

fn run(startup: Handle) -> ServerResult<()> {
    let startup = OwnedHandle::from_handle(startup);
    let started = CrashFileSystemStartup::receive(&PortTransport::server(startup.borrow()))?
        .ok_or(Error::Start)?;
    let backend = |_| {
        Ok(CrashingFileSystem {
            inner: RamFileSystem::new(),
        })
    };
    serve(
        started.log,
        started.control,
        started.service,
        None,
        LOG_TAG,
        backend,
    )
}

#[panic_handler]
fn panic(info: &PanicInfo<'_>) -> ! {
    log_fmt(format_args!("panic: {info}"));
    thread_exit(1)
}
