#![no_std]
#![no_main]

extern crate alloc;

use component_abi::Attributes;
use component_startup::{SessionError, component_manifest};
use filesystem::{
    ByteStore, DirBatch, FileSystemBackend, NodeId, NodeMetadata, RamFileSystem, Window,
};
use filesystem_abi::{DirCursor, FileSystemError, NodeKind, Path, file_system};
use filesystem_server::serve_volumes;
use runtime::{FAILED_EXIT_CODE, exit};
use syscall::{Handle, USER_VA_END, kind};

component_manifest! {
    exports: [file_system::Protocol => [{ fs = "crash" }]],
}

const BUILD_KEY: &[u8] = b"build";
const REFUSED_ONCE: &[u8] = b"refused-once";

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

    fn metadata(&self, node: NodeId) -> Result<NodeMetadata, FileSystemError> {
        self.inner.metadata(node)
    }

    fn read_window(&self, node: NodeId) -> Result<Window, FileSystemError> {
        self.inner.read_window(node)
    }

    fn prepare_write(
        &mut self,
        stores: &mut dyn ByteStore,
        node: NodeId,
        offset: u64,
        len: usize,
    ) -> Result<Window, FileSystemError> {
        self.inner.prepare_write(stores, node, offset, len)
    }

    fn commit_write(&mut self, node: NodeId, end: u64) -> Result<(), FileSystemError> {
        self.inner.commit_write(node, end)
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
    ) -> Result<DirBatch, FileSystemError> {
        self.inner.list(node, from, out)
    }

    fn pin(&mut self, node: NodeId) -> Result<(), FileSystemError> {
        self.inner.pin(node)
    }
}

#[allow(unsafe_code)]
fn kill_process_by_fault() -> ! {
    let unmapped = USER_VA_END as *const u64;
    // SAFETY: адрес намеренно недоступен из EL0 - фикстура проверяет завершение процесса ядром.
    unsafe {
        unmapped.read_volatile();
    }
    exit(FAILED_EXIT_CODE)
}

#[program_entry::component]
fn run(parent: Handle<kind::Port>) -> filesystem_server::Result<()> {
    // Первая заявка с `build=refused-once` получает отказ построителя, следующие строят том.
    let mut refused = false;
    let backend = move |attributes: Attributes<'_>| {
        if !refused && attributes.value(BUILD_KEY) == Some(REFUSED_ONCE) {
            refused = true;
            return Err(SessionError::Refused);
        }
        Ok(CrashingFileSystem {
            inner: RamFileSystem::new(),
        })
    };
    let Err(error) = serve_volumes(parent, backend);
    Err(error)
}
