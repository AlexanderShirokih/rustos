//! Абсолютный путь контракта: непустые компоненты без `.` и `..` в границах `PATH_MAX` и
//! `NAME_MAX`. Разбор идёт по срезу исходной строки, без аллокации.

use crate::{
    error::FileSystemError,
    limits::{NAME_MAX, PATH_MAX},
};

/// Разделитель компонентов пути.
pub const SEPARATOR: char = '/';

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
    pub fn new(text: &'a str) -> Result<Self, FileSystemError> {
        if text.len() > PATH_MAX {
            return Err(FileSystemError::InvalidPath);
        }
        let tail = text
            .strip_prefix(SEPARATOR)
            .ok_or(FileSystemError::InvalidPath)?;
        if !tail.is_empty() {
            for name in tail.split(SEPARATOR) {
                validate_name(name)?;
            }
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

    /// Компоненты от корня. У корня их нет.
    pub fn components(self) -> impl Iterator<Item = Component<'a>> {
        self.0[1..]
            .split(SEPARATOR)
            .filter(|name| !name.is_empty())
            .map(Component)
    }

    /// Родительский путь и последний компонент. У корня их нет.
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

fn validate_name(name: &str) -> Result<(), FileSystemError> {
    if name.is_empty() || name.len() > NAME_MAX || name == "." || name == ".." {
        return Err(FileSystemError::InvalidPath);
    }
    Ok(())
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
    fn root_prefix_matches_every_path() {
        let path = Path::new("/data/sample").expect("valid path");
        assert_eq!(path.strip_prefix(Path::ROOT), Some(path));
        assert_eq!(Path::ROOT.strip_prefix(Path::ROOT), Some(Path::ROOT));
    }
}
