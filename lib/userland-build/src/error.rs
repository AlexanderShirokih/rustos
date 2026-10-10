//! Отказ сборочного скрипта программы userland.
//!
//! Сборочный скрипт возвращает ошибку из `main`, и Rust печатает её через `Debug`. `Debug`
//! ошибки выводит то же сообщение, что `Display`.

use core::{
    error,
    fmt::{self, Debug, Display, Formatter},
};
use std::{env, io, path::PathBuf, process::ExitStatus};

use crate::c_sources::SYSROOT_VARIABLE;

const PREPARE_COMMAND: &str = "cargo xtask libc-src";

/// Причина, по которой сборочный скрипт не подготовил программу.
pub enum Error {
    /// Cargo не передал скрипту переменную окружения.
    MissingVariable(&'static str),
    /// Пакет собирается не через `cargo xtask`, и переменная `MOLECULAR_LIBC_SYSROOT` не задана.
    SysrootVariableUnset,
    /// `cargo xtask libc-src` не собрал библиотеку C, и в sysroot нет файла архитектуры.
    LibcNotBuilt { path: PathBuf },
    /// Путь исходника не задаёт имени объектного файла.
    SourceName(String),
    /// Файл сборки не записан.
    Write { path: PathBuf, source: io::Error },
    /// Инструмент сборки не запустился.
    Spawn { what: String, source: io::Error },
    /// Инструмент сборки завершился неуспешно.
    Failed { what: String, status: ExitStatus },
}

impl Display for Error {
    fn fmt(&self, f: &mut Formatter<'_>) -> fmt::Result {
        match self {
            Self::MissingVariable(name) => write!(f, "cargo did not set {name}"),
            Self::SysrootVariableUnset => write!(
                f,
                "{SYSROOT_VARIABLE} is not set; build the package through `cargo xtask`"
            ),
            Self::LibcNotBuilt { path } => write!(
                f,
                "{} is missing: run `{PREPARE_COMMAND}` to build the C library",
                path.display()
            ),
            Self::SourceName(source) => write!(f, "source `{source}` has no file name"),
            Self::Write { path, source } => {
                write!(f, "failed to write {}: {source}", path.display())
            }
            Self::Spawn { what, source } => write!(f, "failed to run {what}: {source}"),
            Self::Failed { what, status } => write!(f, "{what} failed with {status}"),
        }
    }
}

impl Debug for Error {
    fn fmt(&self, f: &mut Formatter<'_>) -> fmt::Result {
        Display::fmt(self, f)
    }
}

impl error::Error for Error {
    fn source(&self) -> Option<&(dyn error::Error + 'static)> {
        match self {
            Self::Write { source, .. } | Self::Spawn { source, .. } => Some(source),
            _ => None,
        }
    }
}

pub(crate) fn cargo_variable(name: &'static str) -> Result<String, Error> {
    env::var(name).map_err(|_| Error::MissingVariable(name))
}
