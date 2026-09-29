use crate::MAX_PROCESS_NAME_LEN;

/// Непустое имя процесса длиной не более [`MAX_PROCESS_NAME_LEN`] байт UTF-8.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ProcessName<'a>(&'a str);

impl<'a> ProcessName<'a> {
    /// Возвращает `None`, если имя пусто или превышает предел длины.
    pub const fn new(name: &'a str) -> Option<Self> {
        if name.is_empty() || name.len() > MAX_PROCESS_NAME_LEN {
            None
        } else {
            Some(Self(name))
        }
    }

    pub const fn as_str(self) -> &'a str {
        self.0
    }
}

#[cfg(test)]
mod tests {
    use super::{MAX_PROCESS_NAME_LEN, ProcessName};

    #[test]
    fn names_are_bounded_by_utf8_bytes() {
        assert!(ProcessName::new("").is_none());
        assert!(ProcessName::new(&"x".repeat(MAX_PROCESS_NAME_LEN)).is_some());
        assert!(ProcessName::new(&"x".repeat(MAX_PROCESS_NAME_LEN + 1)).is_none());
        assert!(ProcessName::new(&"я".repeat(MAX_PROCESS_NAME_LEN / 2)).is_some());
        assert!(ProcessName::new(&"я".repeat(MAX_PROCESS_NAME_LEN / 2 + 1)).is_none());
    }
}
