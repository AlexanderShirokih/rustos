//! Каталог, который разбирает маршрутизатор, и путь операции внутри корня сессии.

use filesystem_abi::{FileSystemError, Path, SEPARATOR};

use crate::error::Result;

pub(crate) fn within<'a>(path: Path<'a>, root: Path<'a>) -> Result<Path<'a>> {
    Ok(path
        .strip_prefix(root)
        .ok_or(FileSystemError::InvalidPath)?)
}

pub(crate) fn split_scope(within: Path<'_>) -> Result<(Path<'_>, Path<'_>)> {
    // Разделитель остаётся в имени узла, поэтому оба значения - срезы исходного пути.
    // Корню сессии отвечает пара из двух корней, и узлом тогда служит сам корень.
    let cut = within
        .as_str()
        .rfind(SEPARATOR)
        .ok_or(FileSystemError::InvalidPath)?;
    let leaf = within
        .as_str()
        .get(cut..)
        .ok_or(FileSystemError::InvalidPath)?;
    Ok((within.directory(), Path::new(leaf)?))
}
