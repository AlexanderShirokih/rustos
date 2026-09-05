//! Каталог, который разбирает маршрутизатор, и путь операции внутри корня сессии.

use filesystem_abi::{Component, FileSystemError, PATH_MAX, Path, SEPARATOR};
use ipc::wire::Str;

use crate::error::Result;

pub(crate) fn directory_of(path: Path<'_>) -> Path<'_> {
    path.split_last()
        .map_or(Path::ROOT, |(directory, _)| directory)
}

pub(crate) fn common_directory<'a>(first: Path<'a>, second: Path<'_>) -> Result<Path<'a>> {
    let first = directory_of(first);
    let mut names = directory_of(second).components();

    let mut len = 0;
    for name in first.components() {
        if names.next().map(Component::as_str) != Some(name.as_str()) {
            break;
        }
        len += SEPARATOR.len_utf8() + name.as_str().len();
    }
    if len == 0 {
        return Ok(Path::ROOT);
    }

    let text = first
        .as_str()
        .get(..len)
        .ok_or(FileSystemError::InvalidPath)?;
    Ok(Path::new(text)?)
}

pub(crate) fn within<'a>(path: Path<'a>, root: Path<'a>) -> Result<Path<'a>> {
    Ok(path
        .strip_prefix(root)
        .ok_or(FileSystemError::InvalidPath)?)
}

pub(crate) fn wire(path: Path<'_>) -> Result<Str<'_, PATH_MAX>> {
    Ok(Str::new(path.as_str()).ok_or(FileSystemError::InvalidPath)?)
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
    Ok((directory_of(within), Path::new(leaf)?))
}
