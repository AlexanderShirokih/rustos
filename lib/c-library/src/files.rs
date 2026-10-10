//! Операции библиотеки C над путями и каталогами поверх файлового корня процесса.
//!
//! Модуль экспортирует `ftruncate`, `unlink`, `rmdir`, `rename`, `mkdir` и невариадическую часть
//! `stat`, `fstat`, `opendir`, `readdir` и `closedir`, которую `files.c` раскладывает в
//! структуры заголовков. `mkdir` создаёт каталог только под свободным именем. Поток каталога
//! владеет обходом `filesystem-client` с отдельной сессией.

use alloc::{alloc::AllocError, boxed::Box};
use core::{
    ffi::{c_char, c_int, c_uint, c_void},
    ptr, slice,
};

use filesystem_abi::{AccessMode, NodeInfo, NodeKind, OpenFlags};
use filesystem_client::{FileSystemEnvironment, Listing};

use crate::{
    descriptor,
    errno::{Errno, fail},
    root, stdio,
};

// Значения определены в `files.c` константами и не меняются.
#[allow(unsafe_code)]
unsafe extern "C" {
    safe static c_library_s_ifreg: u32;
    safe static c_library_s_ifdir: u32;
    safe static c_library_s_ifchr: u32;

    safe static c_library_dt_reg: u8;
    safe static c_library_dt_dir: u8;
}

#[repr(C)]
struct Node {
    mode: u32,
    size: i64,
}

impl Node {
    fn of(info: NodeInfo) -> Self {
        let mode = match info.kind {
            NodeKind::File => c_library_s_ifreg,
            NodeKind::Directory => c_library_s_ifdir,
        };
        Self {
            mode,
            size: i64::try_from(info.size).unwrap_or(i64::MAX),
        }
    }

    fn console() -> Self {
        Self {
            mode: c_library_s_ifchr,
            size: 0,
        }
    }
}

fn status(result: Result<(), Errno>) -> c_int {
    match result {
        Ok(()) => 0,
        Err(error) => fail(error, -1),
    }
}

#[allow(unsafe_code)]
unsafe fn report(node: Result<Node, Errno>, out: *mut Node) -> c_int {
    match node {
        Ok(node) => {
            // SAFETY: вызывающий передаёт в `out` записываемую `struct c_library_node`.
            unsafe { out.write(node) };
            0
        }
        Err(error) => fail(error, -1),
    }
}

#[allow(unsafe_code)]
#[unsafe(no_mangle)]
unsafe extern "C" fn c_library_stat(path: *const c_char, out: *mut Node) -> c_int {
    // SAFETY: `stat` передаёт путь вызывающего, строку C с завершающим нулём, которая не
    // меняется до возврата.
    let node = unsafe { root::path(path) }
        .and_then(|path| root::with_root(|root| root.metadata(path)))
        .map(Node::of);
    // SAFETY: `stat` передаёт свою `struct c_library_node` на стеке.
    unsafe { report(node, out) }
}

#[allow(unsafe_code)]
#[unsafe(no_mangle)]
unsafe extern "C" fn c_library_fstat(fd: c_int, out: *mut Node) -> c_int {
    let node = if stdio::is_standard(fd) {
        Ok(Node::console())
    } else {
        descriptor::metadata(fd).map(Node::of)
    };
    // SAFETY: `fstat` передаёт свою `struct c_library_node` на стеке.
    unsafe { report(node, out) }
}

#[allow(unsafe_code)]
#[unsafe(no_mangle)]
extern "C" fn ftruncate(fd: c_int, length: i64) -> c_int {
    let size = u64::try_from(length).map_err(|_| Errno::InvalidArgument);
    status(size.and_then(|size| descriptor::set_size(fd, size)))
}

#[allow(unsafe_code)]
#[unsafe(no_mangle)]
unsafe extern "C" fn unlink(path: *const c_char) -> c_int {
    // SAFETY: вызывающий передаёт строку C с завершающим нулём, которая не меняется до
    // возврата.
    let path = unsafe { root::path(path) };
    status(path.and_then(|path| root::with_root(|root| root.remove_file(path))))
}

#[allow(unsafe_code)]
#[unsafe(no_mangle)]
unsafe extern "C" fn rmdir(path: *const c_char) -> c_int {
    // SAFETY: вызывающий передаёт строку C с завершающим нулём, которая не меняется до
    // возврата.
    let path = unsafe { root::path(path) };
    status(path.and_then(|path| root::with_root(|root| root.remove_directory(path))))
}

#[allow(unsafe_code)]
#[unsafe(no_mangle)]
unsafe extern "C" fn mkdir(path: *const c_char, _mode: c_uint) -> c_int {
    // SAFETY: вызывающий передаёт строку C с завершающим нулём, которая не меняется до
    // возврата.
    let path = unsafe { root::path(path) };
    let flags = OpenFlags::CREATE | OpenFlags::EXCLUSIVE | OpenFlags::DIRECTORY;
    status(path.and_then(|path| {
        root::with_root(|root| root.open(path, AccessMode::Read, flags).map(drop))
    }))
}

#[allow(unsafe_code)]
#[unsafe(no_mangle)]
unsafe extern "C" fn rename(from: *const c_char, to: *const c_char) -> c_int {
    // SAFETY: вызывающий передаёт строки C с завершающим нулём, которые не меняются до
    // возврата.
    let from = unsafe { root::path(from) };
    // SAFETY: то же для второго пути.
    let to = unsafe { root::path(to) };
    status(from.and_then(|from| to.and_then(|to| root::with_root(|root| root.rename(from, to)))))
}

#[allow(unsafe_code)]
#[unsafe(no_mangle)]
unsafe extern "C" fn c_library_opendir(path: *const c_char) -> *mut c_void {
    // SAFETY: `opendir` передаёт путь вызывающего, строку C с завершающим нулём, которая не
    // меняется до возврата.
    let path = unsafe { root::path(path) };
    match path.and_then(|path| root::with_root(|root| root.read_directory(path))) {
        Ok(listing) => match Box::try_new(listing) {
            Ok(listing) => Box::into_raw(listing).cast(),
            Err(AllocError) => fail(Errno::OutOfMemory, ptr::null_mut()),
        },
        Err(error) => fail(error, ptr::null_mut()),
    }
}

#[allow(unsafe_code)]
#[unsafe(no_mangle)]
unsafe extern "C" fn c_library_readdir(
    listing: *mut c_void,
    name: *mut c_char,
    capacity: usize,
    kind: *mut u8,
) -> c_int {
    // SAFETY: `readdir` передаёт обход, который вернул `c_library_opendir` и ещё не закрыл
    // `c_library_closedir`; поток каталога библиотеки C использует один поток.
    let listing = unsafe { &mut *listing.cast::<Listing<FileSystemEnvironment>>() };
    let entry = match listing.next_entry() {
        Ok(Some(entry)) => entry,
        Ok(None) => return 0,
        Err(error) => return fail(Errno::from(error), -1),
    };

    let entry_kind = match entry.kind {
        NodeKind::File => c_library_dt_reg,
        NodeKind::Directory => c_library_dt_dir,
    };
    // SAFETY: `name` - массив `d_name` записи потока ёмкостью `capacity` байт, обход его не
    // занимает.
    let name = unsafe { slice::from_raw_parts_mut(name.cast::<u8>(), capacity) };
    let bytes = entry.name.as_bytes();
    let Some((nul, text)) = name
        .get_mut(..=bytes.len())
        .and_then(|name| name.split_last_mut())
    else {
        return fail(Errno::Io, -1);
    };
    text.copy_from_slice(bytes);
    *nul = 0;
    // SAFETY: `kind` - поле `d_type` той же записи.
    unsafe { kind.write(entry_kind) };
    1
}

#[allow(unsafe_code)]
#[unsafe(no_mangle)]
unsafe extern "C" fn c_library_closedir(listing: *mut c_void) {
    // SAFETY: `closedir` передаёт обход, который вернул `c_library_opendir`, один раз.
    drop(unsafe { Box::from_raw(listing.cast::<Listing<FileSystemEnvironment>>()) });
}
