//! Абсолютный путь контракта: непустые компоненты без `.` и `..` в границах `PATH_MAX` и
//! `NAME_MAX`. Разбор идёт по срезу исходной строки, без аллокации.

use crate::{
    error::FileSystemError,
    limits::{NAME_MAX, PATH_MAX},
};

/// Разделитель компонентов пути.
pub const SEPARATOR: char = '/';

const SEPARATOR_BYTE: u8 = b'/';

const _: () = assert!(
    SEPARATOR as u32 == SEPARATOR_BYTE as u32,
    "the separator byte differs from the separator"
);

/// Абсолютный путь, форму и границы которого проверил [`Path::new`].
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Path<'a>(&'a str);

/// Компонент пути.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Component<'a>(&'a str);

impl<'a> Path<'a> {
    /// Корень дерева имён.
    pub const ROOT: Self = Self("/");

    /// Проверяет форму и границы пути. Нарушение любого правила - `InvalidPath`.
    /// Константный путь программы проверяется на сборке.
    pub const fn new(text: &'a str) -> Result<Self, FileSystemError> {
        if text.len() > PATH_MAX {
            return Err(FileSystemError::InvalidPath);
        }
        let [SEPARATOR_BYTE, tail @ ..] = text.as_bytes() else {
            return Err(FileSystemError::InvalidPath);
        };
        if !tail.is_empty() && !validate_names(tail) {
            return Err(FileSystemError::InvalidPath);
        }
        Ok(Self(text))
    }

    /// Путь целиком, включая ведущий разделитель.
    pub const fn as_str(self) -> &'a str {
        self.0
    }

    pub const fn is_root(self) -> bool {
        self.0.len() == 1
    }

    /// Компоненты пути от корня. У корня перечисление пусто.
    pub fn components(self) -> impl Iterator<Item = Component<'a>> {
        self.0[1..]
            .split(SEPARATOR)
            .filter(|name| !name.is_empty())
            .map(Component)
    }

    /// Каталог, которому принадлежит путь. Для корневого каталога возвращает сам корень.
    pub fn directory(self) -> Self {
        self.split_last()
            .map_or(Self::ROOT, |(directory, _)| directory)
    }

    /// Родительский путь и последний компонент. Для корня - `None`.
    pub fn split_last(self) -> Option<(Self, Component<'a>)> {
        if self.is_root() {
            return None;
        }
        let cut = self.0.rfind(SEPARATOR)?;
        let parent = if cut == 0 {
            Self::ROOT
        } else {
            Self(&self.0[..cut])
        };
        Some((parent, Component(&self.0[cut + 1..])))
    }

    /// Остаток пути после `prefix`, совпавшего по границе компонента. Остаток самого префикса -
    /// корень, а `None` означает, что путь лежит вне префикса.
    pub fn strip_prefix(self, prefix: Self) -> Option<Self> {
        if prefix.is_root() {
            return Some(self);
        }
        let rest = self.0.strip_prefix(prefix.0)?;
        if rest.is_empty() {
            return Some(Self::ROOT);
        }
        rest.starts_with(SEPARATOR).then_some(Self(rest))
    }
}

impl<'a> Component<'a> {
    pub const fn as_str(self) -> &'a str {
        self.0
    }
}

/// Полный путь `tail`, пересаженный под окно `scope`. Корнем результата становится `scope`, а
/// корень `tail` называет само окно. Граница `PATH_MAX` принадлежит полному пути, и окно,
/// удлинившее путь за предел, даёт `InvalidPath`. Результат хранится в `out`.
pub fn rebase<'a>(
    scope: Path<'_>,
    tail: Path<'_>,
    out: &'a mut [u8; PATH_MAX],
) -> Result<Path<'a>, FileSystemError> {
    let scope = if scope.is_root() { "" } else { scope.as_str() };
    let tail = if tail.is_root() { "" } else { tail.as_str() };
    let len = scope.len() + tail.len();
    if len > PATH_MAX {
        return Err(FileSystemError::InvalidPath);
    }
    if len == 0 {
        return Ok(Path::ROOT);
    }
    let joined = out.get_mut(..len).ok_or(FileSystemError::InvalidPath)?;
    let (scope_out, tail_out) = joined
        .split_at_mut_checked(scope.len())
        .ok_or(FileSystemError::InvalidPath)?;
    scope_out.copy_from_slice(scope.as_bytes());
    tail_out.copy_from_slice(tail.as_bytes());
    let joined = core::str::from_utf8(joined).map_err(|_| FileSystemError::InvalidPath)?;
    Path::new(joined)
}

/// Ближайший общий каталог двух путей: совпадающие компоненты их каталогов. Пути без общего
/// компонента сходятся в корне.
pub fn common_directory<'a>(
    first: Path<'a>,
    second: Path<'_>,
) -> Result<Path<'a>, FileSystemError> {
    let first = first.directory();
    let mut names = second.directory().components();

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
    Path::new(text)
}

const fn validate_names(tail: &[u8]) -> bool {
    let mut rest = tail;
    let mut len = 0;
    let mut dots = 0;
    loop {
        let Some((&byte, next)) = rest.split_first() else {
            return valid_name(len, dots);
        };
        if byte == SEPARATOR_BYTE {
            if !valid_name(len, dots) {
                return false;
            }
            len = 0;
            dots = 0;
        } else {
            len += 1;
            if byte == b'.' {
                dots += 1;
            }
        }
        rest = next;
    }
}

const fn valid_name(len: usize, dots: usize) -> bool {
    // Имя целиком из одной либо двух точек - это `.` и `..`, которых путь контракта не допускает.
    len != 0 && len <= NAME_MAX && !(dots == len && len <= 2)
}

#[cfg(test)]
mod tests {
    use std::{string::String, vec::Vec};

    use super::{Component, Path};
    use crate::{
        error::FileSystemError,
        limits::{NAME_MAX, PATH_MAX},
    };

    fn components(text: &str) -> Vec<&str> {
        Path::new(text)
            .expect("valid path")
            .components()
            .map(Component::as_str)
            .collect()
    }

    fn rejected(text: &str) -> bool {
        Path::new(text) == Err(FileSystemError::InvalidPath)
    }

    #[test]
    fn absolute_path_splits_into_components() {
        assert_eq!(components("/boot/init"), ["boot", "init"]);
        assert_eq!(components("/data"), ["data"]);
    }

    #[test]
    fn a_constant_path_is_checked_at_build_time() {
        const SAMPLE: Path<'static> = match Path::new("/data/sample") {
            Ok(path) => path,
            Err(_) => panic!("the constant path keeps the form of the contract"),
        };

        assert_eq!(SAMPLE.as_str(), "/data/sample");
        assert_eq!(components(SAMPLE.as_str()), ["data", "sample"]);
    }

    #[test]
    fn root_has_no_components() {
        let root = Path::new("/").expect("root is valid");
        assert!(root.is_root());
        assert_eq!(root.components().count(), 0);
        assert_eq!(root, Path::ROOT);
    }

    #[test]
    fn relative_and_empty_paths_are_rejected() {
        assert!(rejected(""));
        assert!(rejected("data"));
        assert!(rejected("data/sample"));
    }

    #[test]
    fn empty_component_is_rejected() {
        assert!(rejected("//"));
        assert!(rejected("/data/"));
        assert!(rejected("/data//sample"));
    }

    #[test]
    fn dot_components_are_rejected() {
        assert!(rejected("/."));
        assert!(rejected("/.."));
        assert!(rejected("/data/./sample"));
        assert!(rejected("/data/../sample"));
        assert!(!rejected("/data/...sample"));
    }

    #[test]
    fn path_length_bound_is_enforced() {
        // Путь набирается компонентами по `NAME_MAX`, иначе первой сработает граница имени.
        let component = "p".repeat(NAME_MAX);
        let mut at_bound = String::new();
        while at_bound.len() < PATH_MAX {
            let room = PATH_MAX - at_bound.len() - 1;
            at_bound.push('/');
            at_bound.push_str(&component[..room.min(NAME_MAX)]);
        }
        assert_eq!(at_bound.len(), PATH_MAX);
        assert!(!rejected(&at_bound));
        assert!(rejected(&(at_bound + "p")));
    }

    #[test]
    fn name_length_bound_is_enforced() {
        let at_bound = String::from("/") + &"n".repeat(NAME_MAX);
        assert!(!rejected(&at_bound));
        assert!(rejected(&(at_bound + "n")));
    }

    #[test]
    fn split_last_gives_parent_and_name() {
        let (parent, name) = Path::new("/data/sample")
            .expect("valid path")
            .split_last()
            .expect("path below root");
        assert_eq!((parent.as_str(), name.as_str()), ("/data", "sample"));

        let (parent, name) = Path::new("/data")
            .expect("valid path")
            .split_last()
            .expect("path below root");
        assert_eq!((parent.as_str(), name.as_str()), ("/", "data"));

        assert_eq!(Path::ROOT.split_last(), None);
    }

    #[test]
    fn strip_prefix_matches_on_component_boundary() {
        let boot = Path::new("/boot").expect("valid path");
        let strip = |text: &'static str| {
            Path::new(text)
                .expect("valid path")
                .strip_prefix(boot)
                .map(Path::as_str)
        };
        assert_eq!(strip("/boot/init"), Some("/init"));
        assert_eq!(strip("/boot"), Some("/"));
        assert_eq!(strip("/bootstrap"), None);
        assert_eq!(strip("/data/sample"), None);
    }

    #[test]
    fn directory_drops_the_last_component() {
        let directory =
            |text: &'static str| Path::new(text).expect("valid path").directory().as_str();
        assert_eq!(directory("/data/sample"), "/data");
        assert_eq!(directory("/data"), "/");
        assert_eq!(directory("/"), "/");
    }

    fn common(first: &str, second: &str) -> Result<String, FileSystemError> {
        let first = Path::new(first).expect("valid path");
        let second = Path::new(second).expect("valid path");
        super::common_directory(first, second).map(|path| String::from(path.as_str()))
    }

    #[test]
    fn common_directory_keeps_the_shared_components() {
        assert_eq!(
            common("/data/a/first", "/data/b/second").as_deref(),
            Ok("/data")
        );
        assert_eq!(
            common("/data/a/first", "/data/a/second").as_deref(),
            Ok("/data/a")
        );
    }

    #[test]
    fn common_directory_falls_back_to_the_root() {
        assert_eq!(common("/a/first", "/b/second").as_deref(), Ok("/"));
        assert_eq!(common("/first", "/second").as_deref(), Ok("/"));
        // Каталог одной стороны лежит выше другой, и общим остаётся корень.
        assert_eq!(common("/data", "/data/b/second").as_deref(), Ok("/"));
    }

    #[test]
    fn common_directory_does_not_split_a_shared_name_prefix() {
        assert_eq!(common("/datastore/a", "/data/b").as_deref(), Ok("/"));
    }

    #[test]
    fn root_prefix_matches_every_path() {
        let path = Path::new("/data/sample").expect("valid path");
        assert_eq!(path.strip_prefix(Path::ROOT), Some(path));
        assert_eq!(Path::ROOT.strip_prefix(Path::ROOT), Some(Path::ROOT));
    }

    fn rebased(scope: &str, tail: &str) -> Result<String, FileSystemError> {
        let scope = Path::new(scope).expect("valid scope");
        let tail = Path::new(tail).expect("valid tail");
        let mut out = [0; PATH_MAX];
        super::rebase(scope, tail, &mut out).map(|path| String::from(path.as_str()))
    }

    #[test]
    fn rebase_puts_the_tail_under_the_scope() {
        assert_eq!(
            rebased("/window", "/data/sample").as_deref(),
            Ok("/window/data/sample")
        );
        assert_eq!(rebased("/a/b", "/c").as_deref(), Ok("/a/b/c"));
    }

    #[test]
    fn rebase_maps_the_tail_root_to_the_scope_itself() {
        assert_eq!(rebased("/window", "/").as_deref(), Ok("/window"));
        assert_eq!(rebased("/", "/").as_deref(), Ok("/"));
    }

    #[test]
    fn rebase_under_the_root_scope_keeps_the_tail() {
        assert_eq!(rebased("/", "/data/sample").as_deref(), Ok("/data/sample"));
    }

    #[test]
    fn rebase_rejects_a_path_past_the_bound() {
        // Оба пути в границах, а их сумма - нет. Предел принадлежит полному пути.
        let scope = String::from("/") + &"s".repeat(NAME_MAX);
        let mut tail = String::new();
        while tail.len() + scope.len() <= PATH_MAX {
            tail.push('/');
            tail.push_str(&"t".repeat(NAME_MAX));
        }
        assert_eq!(rebased(&scope, &tail), Err(FileSystemError::InvalidPath));
        // Без окна тот же хвост валиден: его длина в границах.
        assert!(rebased("/", &tail).is_ok());
    }
}
