//! Переменная `errno` библиотеки C и запись в неё кодов системного слоя.
//!
//! Библиотека C читает и пишет `errno` по адресу из [`__errno_location`]: у каждого потока
//! своя `errno` в его записи. Значения кодов приходят из её заголовков через `errno.c`. Отказ
//! файловой системы получает код POSIX того же смысла, отказ транспорта и отказ без такого
//! кода - `EIO`.

#![allow(unsafe_code)]

use core::ffi::c_int;

use filesystem_abi::FileSystemError;
use filesystem_client::Failure;

use crate::thread;

// Значения определены в `errno.c` константами `int` и не меняются.
unsafe extern "C" {
    safe static c_library_ebadf: c_int;
    safe static c_library_einval: c_int;
    safe static c_library_eio: c_int;
    safe static c_library_enomem: c_int;
    safe static c_library_espipe: c_int;
    safe static c_library_enoent: c_int;
    safe static c_library_enotdir: c_int;
    safe static c_library_eisdir: c_int;
    safe static c_library_enotempty: c_int;
    safe static c_library_eexist: c_int;
    safe static c_library_emfile: c_int;
    safe static c_library_erofs: c_int;
    safe static c_library_enospc: c_int;
    safe static c_library_exdev: c_int;
    safe static c_library_enotsup: c_int;
    safe static c_library_etimedout: c_int;
    safe static c_library_enotconn: c_int;
    safe static c_library_eagain: c_int;
    safe static c_library_edeadlk: c_int;
    safe static c_library_ebusy: c_int;
    safe static c_library_eperm: c_int;
    safe static c_library_enosys: c_int;
}

#[derive(Clone, Copy)]
pub(crate) enum Errno {
    BadDescriptor,
    InvalidArgument,
    Io,
    OutOfMemory,
    IllegalSeek,
    NotFound,
    NotADirectory,
    IsADirectory,
    NotEmpty,
    Exists,
    TooManyFiles,
    ReadOnly,
    NoSpace,
    CrossDevice,
    NotSupported,
    TimedOut,
    NotConnected,
    Again,
    Deadlock,
    Busy,
    NotPermitted,
    NoSystem,
}

impl Errno {
    pub(crate) fn code(self) -> c_int {
        match self {
            Self::BadDescriptor => c_library_ebadf,
            Self::InvalidArgument => c_library_einval,
            Self::Io => c_library_eio,
            Self::OutOfMemory => c_library_enomem,
            Self::IllegalSeek => c_library_espipe,
            Self::NotFound => c_library_enoent,
            Self::NotADirectory => c_library_enotdir,
            Self::IsADirectory => c_library_eisdir,
            Self::NotEmpty => c_library_enotempty,
            Self::Exists => c_library_eexist,
            Self::TooManyFiles => c_library_emfile,
            Self::ReadOnly => c_library_erofs,
            Self::NoSpace => c_library_enospc,
            Self::CrossDevice => c_library_exdev,
            Self::NotSupported => c_library_enotsup,
            Self::TimedOut => c_library_etimedout,
            Self::NotConnected => c_library_enotconn,
            Self::Again => c_library_eagain,
            Self::Deadlock => c_library_edeadlk,
            Self::Busy => c_library_ebusy,
            Self::NotPermitted => c_library_eperm,
            Self::NoSystem => c_library_enosys,
        }
    }
}

impl From<Failure> for Errno {
    fn from(failure: Failure) -> Self {
        match failure {
            Failure::FileSystem(error) => Self::from(error),
            Failure::Transport(_) => Self::Io,
        }
    }
}

impl From<FileSystemError> for Errno {
    fn from(error: FileSystemError) -> Self {
        match error {
            FileSystemError::NotFound => Self::NotFound,
            FileSystemError::NotADirectory => Self::NotADirectory,
            FileSystemError::IsADirectory => Self::IsADirectory,
            FileSystemError::NotEmpty => Self::NotEmpty,
            FileSystemError::AlreadyExists => Self::Exists,
            FileSystemError::InvalidPath => Self::InvalidArgument,
            FileSystemError::TooManyOpenFiles => Self::TooManyFiles,
            FileSystemError::ReadOnly => Self::ReadOnly,
            FileSystemError::NoSpace => Self::NoSpace,
            FileSystemError::CrossMount => Self::CrossDevice,
            FileSystemError::NotSupported => Self::NotSupported,
            FileSystemError::Unresponsive => Self::TimedOut,
            FileSystemError::WrongMode => Self::BadDescriptor,
            FileSystemError::BadDescriptor
            | FileSystemError::NoSession
            | FileSystemError::NoBuffer
            | FileSystemError::BufferTooSmall => Self::Io,
        }
    }
}

pub(crate) fn set(error: Errno) {
    let code = error.code();
    // SAFETY: адрес указывает на `errno` записи текущего потока, и её пишет только он.
    unsafe { __errno_location().write(code) };
}

pub(crate) fn fail<T>(error: Errno, value: T) -> T {
    set(error);
    value
}

#[unsafe(no_mangle)]
extern "C" fn __errno_location() -> *mut c_int {
    thread::current().errno()
}
