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

pub(crate) fn within<'a>(path: Path<'a>, root: Path<'a>) -> Result<Str<'a, PATH_MAX>> {
    let tail = path
        .strip_prefix(root)
        .ok_or(FileSystemError::InvalidPath)?;
    Ok(Str::new(tail.as_str()).ok_or(FileSystemError::InvalidPath)?)
}
