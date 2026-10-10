//! Таблица дескрипторов библиотеки C и экспорты ввода-вывода по дескриптору.
//!
//! Дескрипторы 0-2 - стандартные потоки [`stdio`]. Дескриптор от 3 - запись таблицы с открытым
//! узлом файловой системы, его позицией и признаком `O_APPEND`. `open` занимает наименьшую
//! свободную запись, `close` освобождает её и закрывает узел. `open` переводит режим доступа,
//! `O_CREAT`, `O_EXCL` и `O_TRUNC` в запрос открытия, а `O_APPEND` - в признак записи.
//! `O_CLOEXEC`, `O_NOCTTY`, `O_NONBLOCK` и `O_SYNC` вызов пропускает, иной бит даёт `EINVAL`.
//!
//! Размер для `SEEK_END` и `fstat` файловая система отдаёт только дескриптору с правом чтения,
//! и на дескрипторе без него оба вызова дают `EBADF`.

use alloc::vec::Vec;
use core::{
    ffi::{c_char, c_int, c_void},
    slice,
};

use filesystem_abi::{AccessMode, NodeInfo, OpenFlags};
use filesystem_client::{FileSystemEnvironment, OpenNode};
use runtime::Mutex;

use crate::{
    errno::{Errno, fail},
    root, stdio,
};

const FIRST_FILE: c_int = 3;

static FILES: Mutex<Vec<Option<File>>> = Mutex::new(Vec::new());

// Значения определены в `files.c` константами `int` и не меняются.
#[allow(unsafe_code)]
unsafe extern "C" {
    safe static c_library_o_accmode: c_int;
    safe static c_library_o_rdonly: c_int;
    safe static c_library_o_wronly: c_int;
    safe static c_library_o_rdwr: c_int;
    safe static c_library_o_creat: c_int;
    safe static c_library_o_excl: c_int;
    safe static c_library_o_trunc: c_int;
    safe static c_library_o_append: c_int;
    safe static c_library_o_ignored: c_int;

    safe static c_library_seek_set: c_int;
    safe static c_library_seek_cur: c_int;
    safe static c_library_seek_end: c_int;
}

struct File {
    node: OpenNode<FileSystemEnvironment>,
    append: bool,
}

enum Descriptor {
    Standard(c_int),
    File(usize),
}

struct OpenRequest {
    mode: AccessMode,
    flags: OpenFlags,
    append: bool,
}

impl File {
    fn write(&mut self, src: &[u8]) -> Result<usize, Errno> {
        if self.append {
            return self.append(src);
        }
        self.node.write(src).map_err(Errno::from)
    }

    fn append(&mut self, src: &[u8]) -> Result<usize, Errno> {
        if src.is_empty() {
            return Ok(0);
        }
        // Вызов делает одну дозапись, поэтому чужая дозапись не встаёт внутрь его данных. Остаток
        // длиннее буфера сессии вызывающий пишет следующей `write`.
        let written = self.node.append(src)?;
        // Дозапись ставит позицию за записанные байты, как `O_APPEND` на Unix.
        self.node
            .seek(written.offset.saturating_add(u64::from(written.count)));
        Ok(usize::try_from(written.count)
            .unwrap_or(src.len())
            .min(src.len()))
    }

    fn seek(&mut self, offset: i64, whence: c_int) -> Result<i64, Errno> {
        let base = if whence == c_library_seek_set {
            0
        } else if whence == c_library_seek_cur {
            self.node.position()
        } else if whence == c_library_seek_end {
            self.node.metadata()?.size
        } else {
            return Err(Errno::InvalidArgument);
        };

        let position = base
            .checked_add_signed(offset)
            .and_then(|position| i64::try_from(position).ok())
            .ok_or(Errno::InvalidArgument)?;
        self.node.seek(position.cast_unsigned());
        Ok(position)
    }
}

impl Descriptor {
    fn of(fd: c_int) -> Result<Self, Errno> {
        if stdio::is_standard(fd) {
            return Ok(Self::Standard(fd));
        }
        fd.checked_sub(FIRST_FILE)
            .and_then(|index| usize::try_from(index).ok())
            .map(Self::File)
            .ok_or(Errno::BadDescriptor)
    }
}

impl OpenRequest {
    fn of(flags: c_int) -> Result<Self, Errno> {
        let known = c_library_o_accmode
            | c_library_o_creat
            | c_library_o_excl
            | c_library_o_trunc
            | c_library_o_append
            | c_library_o_ignored;
        if flags & !known != 0 {
            return Err(Errno::InvalidArgument);
        }

        let set = |flag: c_int| flags & flag != 0;
        let mut action = OpenFlags::NONE;
        if set(c_library_o_creat) {
            action = action | OpenFlags::CREATE;
            if set(c_library_o_excl) {
                action = action | OpenFlags::EXCLUSIVE;
            }
        }
        if set(c_library_o_trunc) {
            action = action | OpenFlags::TRUNCATE;
        }

        Ok(Self {
            mode: access_mode(flags)?,
            flags: action,
            append: set(c_library_o_append),
        })
    }
}

pub(crate) fn metadata(fd: c_int) -> Result<NodeInfo, Errno> {
    on_descriptor(
        fd,
        |_| Err(Errno::BadDescriptor),
        |file| Ok(file.node.metadata()?),
    )
}

pub(crate) fn set_size(fd: c_int, size: u64) -> Result<(), Errno> {
    on_descriptor(
        fd,
        |_| Err(Errno::InvalidArgument),
        |file| Ok(file.node.set_size(size)?),
    )
}

fn on_descriptor<T>(
    fd: c_int,
    standard: impl FnOnce(c_int) -> Result<T, Errno>,
    file: impl FnOnce(&mut File) -> Result<T, Errno>,
) -> Result<T, Errno> {
    match Descriptor::of(fd)? {
        Descriptor::Standard(fd) => standard(fd),
        Descriptor::File(index) => with_file(index, file),
    }
}

fn with_file<T>(
    index: usize,
    operation: impl FnOnce(&mut File) -> Result<T, Errno>,
) -> Result<T, Errno> {
    let mut files = FILES.lock();
    let file = files
        .get_mut(index)
        .and_then(Option::as_mut)
        .ok_or(Errno::BadDescriptor)?;
    operation(file)
}

fn insert(file: File) -> Result<c_int, Errno> {
    let mut files = FILES.lock();
    let index = if let Some(index) = files.iter().position(Option::is_none) {
        index
    } else {
        files.try_reserve(1).map_err(|_| Errno::OutOfMemory)?;
        files.push(None);
        files.len() - 1
    };

    let fd = c_int::try_from(index)
        .ok()
        .and_then(|index| index.checked_add(FIRST_FILE))
        .ok_or(Errno::TooManyFiles)?;
    let slot = files.get_mut(index).ok_or(Errno::TooManyFiles)?;
    *slot = Some(file);
    Ok(fd)
}

fn access_mode(flags: c_int) -> Result<AccessMode, Errno> {
    let access = flags & c_library_o_accmode;
    if access == c_library_o_rdonly {
        Ok(AccessMode::Read)
    } else if access == c_library_o_wronly {
        Ok(AccessMode::Write)
    } else if access == c_library_o_rdwr {
        Ok(AccessMode::ReadWrite)
    } else {
        Err(Errno::InvalidArgument)
    }
}

#[allow(unsafe_code)]
unsafe fn source<'a>(buf: *const c_void, count: usize) -> &'a [u8] {
    if count == 0 {
        return &[];
    }
    // SAFETY: по `buf` лежат `count` читаемых байт (предусловие функции), `transfer` ограничил
    // `count` значением `isize::MAX`.
    unsafe { slice::from_raw_parts(buf.cast::<u8>(), count) }
}

#[allow(unsafe_code)]
unsafe fn destination<'a>(buf: *mut c_void, count: usize) -> &'a mut [u8] {
    if count == 0 {
        return &mut [];
    }
    // SAFETY: по `buf` лежат `count` записываемых байт (предусловие функции), `transfer`
    // ограничил `count` значением `isize::MAX`.
    unsafe { slice::from_raw_parts_mut(buf.cast::<u8>(), count) }
}

fn transfer(count: usize, operation: impl FnOnce() -> Result<usize, Errno>) -> isize {
    let transferred = isize::try_from(count)
        .map_err(|_| Errno::InvalidArgument)
        .and_then(|_| operation());
    match transferred {
        Ok(transferred) => isize::try_from(transferred).unwrap_or(isize::MAX),
        Err(error) => fail(error, -1),
    }
}

#[allow(unsafe_code)]
#[unsafe(no_mangle)]
unsafe extern "C" fn c_library_open(path: *const c_char, flags: c_int) -> c_int {
    // SAFETY: `open` передаёт путь вызывающего, строку C с завершающим нулём, которая не
    // меняется до возврата.
    let path = match unsafe { root::path(path) } {
        Ok(path) => path,
        Err(error) => return fail(error, -1),
    };

    let opened = OpenRequest::of(flags).and_then(|request| {
        let node = root::with_root(|root| root.open(path, request.mode, request.flags))?;
        insert(File {
            node,
            append: request.append,
        })
    });
    opened.unwrap_or_else(|error| fail(error, -1))
}

#[allow(unsafe_code)]
#[unsafe(no_mangle)]
unsafe extern "C" fn write(fd: c_int, buf: *const c_void, count: usize) -> isize {
    transfer(count, || {
        // SAFETY: вызывающий передаёт `count` читаемых байт по `buf`.
        let src = unsafe { source(buf, count) };
        on_descriptor(fd, |fd| stdio::write(fd, src), |file| file.write(src))
    })
}

#[allow(unsafe_code)]
#[unsafe(no_mangle)]
unsafe extern "C" fn read(fd: c_int, buf: *mut c_void, count: usize) -> isize {
    transfer(count, || {
        // SAFETY: вызывающий передаёт `count` записываемых байт по `buf`.
        let dst = unsafe { destination(buf, count) };
        on_descriptor(fd, stdio::read, |file| Ok(file.node.read(dst)?))
    })
}

#[allow(unsafe_code)]
#[unsafe(no_mangle)]
extern "C" fn lseek(fd: c_int, offset: i64, whence: c_int) -> i64 {
    on_descriptor(
        fd,
        |_| Err(Errno::IllegalSeek),
        |file| file.seek(offset, whence),
    )
    .unwrap_or_else(|error| fail(error, -1))
}

#[allow(unsafe_code)]
#[unsafe(no_mangle)]
extern "C" fn close(fd: c_int) -> c_int {
    match Descriptor::of(fd) {
        Ok(Descriptor::Standard(_)) => 0,
        Ok(Descriptor::File(index)) => {
            // Узел закрывается вызовом к серверу после снятия блокировки таблицы.
            let file = FILES.lock().get_mut(index).and_then(Option::take);
            match file {
                Some(_) => 0,
                None => fail(Errno::BadDescriptor, -1),
            }
        }
        Err(error) => fail(error, -1),
    }
}
