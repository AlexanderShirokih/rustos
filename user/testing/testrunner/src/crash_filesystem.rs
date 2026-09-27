#![no_std]
#![no_main]
#![allow(unsafe_code)]

runtime::process_heap!();

extern crate alloc;

use core::{convert::Infallible, panic::PanicInfo};

use component_startup::component_manifest;
use filesystem::{ByteStore, FileSystemBackend, NodeId, RamFileSystem, Window};
use filesystem_abi::{
    DirCursor, FileSystemError, Listed, NodeInfo, NodeKind, Path, file_system, namespace_admin,
};
use filesystem_server::{Error, Result as ServerResult, serve};
use runtime::{log_fmt, process_exit, start_component};
use syscall::{Handle, USER_VA_END, kind};

const LOG_TAG: &str = "crashfs";

const FAILED_EXIT_CODE: u64 = 1;

component_manifest! {
    struct CrashFileSystemStartup {
        requirements: { namespace: session(namespace_admin::Protocol) },
        exports: [file_system::Protocol],
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

    fn unlink(
        &mut self,
        stores: &mut dyn ByteStore,
        path: Path<'_>,
        expected: NodeKind,
    ) -> Result<(), FileSystemError> {
        self.inner.unlink(stores, path, expected)
    }

    fn rename(
        &mut self,
        _stores: &mut dyn ByteStore,
        _from: Path<'_>,
        _to: Path<'_>,
    ) -> Result<(), FileSystemError> {
        kill_process_by_fault()
    }

    fn retain(&mut self, node: NodeId) -> Result<(), FileSystemError> {
        self.inner.retain(node)
    }

    fn release(&mut self, stores: &mut dyn ByteStore, node: NodeId) {
        self.inner.release(stores, node);
    }

    fn metadata(&self, node: NodeId) -> Result<NodeInfo, FileSystemError> {
        self.inner.metadata(node)
    }

    fn read_at(
        &self,
        stores: &dyn ByteStore,
        node: NodeId,
        offset: u64,
        out: &mut [u8],
    ) -> Result<usize, FileSystemError> {
        self.inner.read_at(stores, node, offset, out)
    }

    fn write_at(
        &mut self,
        stores: &mut dyn ByteStore,
        node: NodeId,
        offset: u64,
        data: &[u8],
    ) -> Result<usize, FileSystemError> {
        self.inner.write_at(stores, node, offset, data)
    }

    fn append(
        &mut self,
        stores: &mut dyn ByteStore,
        node: NodeId,
        data: &[u8],
    ) -> Result<u64, FileSystemError> {
        self.inner.append(stores, node, data)
    }

    fn set_size(
        &mut self,
        stores: &mut dyn ByteStore,
        node: NodeId,
        size: u64,
    ) -> Result<(), FileSystemError> {
        self.inner.set_size(stores, node, size)
    }

    fn list(
        &self,
        node: NodeId,
        from: DirCursor,
        out: &mut [u8],
    ) -> Result<Listed, FileSystemError> {
        self.inner.list(node, from, out)
    }

    fn backing(
        &mut self,
        stores: &mut dyn ByteStore,
        node: NodeId,
    ) -> Result<Window, FileSystemError> {
        self.inner.backing(stores, node)
    }

    fn pin(&mut self, node: NodeId) -> Result<(), FileSystemError> {
        self.inner.pin(node)
    }
}

fn kill_process_by_fault() -> ! {
    let unmapped = USER_VA_END as *const u64;
    // SAFETY: адрес намеренно недоступен из EL0 - фикстура проверяет завершение процесса ядром.
    unsafe {
        unmapped.read_volatile();
    }
    process_exit(FAILED_EXIT_CODE)
}

#[unsafe(no_mangle)]
pub extern "C" fn _start(parent: Handle<kind::Port>) -> ! {
    let Err(error) = run(parent);
    log_fmt(format_args!("stopped: {error}"));
    process_exit(FAILED_EXIT_CODE)
}

fn run(parent: Handle<kind::Port>) -> ServerResult<Infallible> {
    start_component(parent, LOG_TAG);
    let requirements = CrashFileSystemStartup::request(parent).map_err(|_| Error::Start)?;
    let backend = |_| {
        Ok(CrashingFileSystem {
            inner: RamFileSystem::new(),
        })
    };
    serve(parent, requirements.namespace, None, backend)
}

#[panic_handler]
fn panic(info: &PanicInfo<'_>) -> ! {
    log_fmt(format_args!("panic: {info}"));
    process_exit(FAILED_EXIT_CODE)
}
