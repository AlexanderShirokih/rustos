//! Файловый корень процесса: фасад `filesystem-client` поверх сессии пространства имён.
//!
//! Пути C-программы абсолютные и разрешаются от области этой сессии. До установки корня
//! операция по пути отвечает `ENOTCONN`.

use core::ffi::{CStr, c_char};

use component_startup::Session;
use filesystem_abi::{Path, namespace};
use filesystem_client::{FileSystem, FileSystemEnvironment};
use runtime::Mutex;

use crate::errno::Errno;

type Root = FileSystem<FileSystemEnvironment>;

static ROOT: Mutex<Option<Root>> = Mutex::new(None);

/// Делает сессию пространства имён файловым корнем процесса. Установленный корень не
/// заменяется, и повторный вызов возвращает `root` в `Err`.
///
/// Корень должен быть установлен до первой операции с путём. Программа с требованиями
/// объявляет Rust-функцию `main`, которую рантайм вызывает как C-функцию входа. Эта функция
/// открывает сессию требования из [`requirements`](crate::requirements), устанавливает корень и
/// передаёт управление C-коду. Конструкторы библиотеки C выполняются до `main`, и операции с
/// путями в них дают `ENOTCONN`.
pub fn install_filesystem_root(
    root: Session<namespace::Protocol>,
) -> Result<(), Session<namespace::Protocol>> {
    let mut cell = ROOT.lock();
    if cell.is_some() {
        return Err(root);
    }

    *cell = Some(FileSystem::new(root));
    Ok(())
}

pub(crate) fn with_root<T>(
    operation: impl FnOnce(&mut Root) -> filesystem_client::Result<T>,
) -> Result<T, Errno> {
    let mut cell = ROOT.lock();
    let root = cell.as_mut().ok_or(Errno::NotConnected)?;
    operation(root).map_err(Errno::from)
}

#[allow(unsafe_code)]
pub(crate) unsafe fn path<'a>(path: *const c_char) -> Result<Path<'a>, Errno> {
    // SAFETY: вызывающий передаёт строку C с завершающим нулём, которая остаётся доступной и
    // неизменной в течение `'a`.
    let text = unsafe { CStr::from_ptr(path) };
    let text = text.to_str().map_err(|_| Errno::InvalidArgument)?;
    Path::new(text).map_err(Errno::from)
}
