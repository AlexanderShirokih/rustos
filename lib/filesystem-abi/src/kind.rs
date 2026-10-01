ipc::wire_enum! {
    /// Вид узла дерева имён: файл либо каталог.
    #[derive(Debug, Clone, Copy, PartialEq, Eq)]
    pub enum NodeKind: u8 {
        File = 1,
        Directory = 2,
    }
}

impl NodeKind {
    #[must_use]
    pub const fn is_directory(self) -> bool {
        matches!(self, Self::Directory)
    }
}

#[cfg(test)]
mod tests {
    use super::NodeKind;

    #[test]
    fn only_the_directory_is_a_directory() {
        assert!(NodeKind::Directory.is_directory());
        assert!(!NodeKind::File.is_directory());
    }
}
