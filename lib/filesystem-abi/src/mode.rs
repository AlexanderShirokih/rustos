ipc::wire_enum! {
    /// Режим доступа дескриптора. Обязательный параметр `open`: дескриптор без права на чтение и
    /// запись невыразим.
    #[derive(Debug, Clone, Copy, PartialEq, Eq)]
    pub enum AccessMode: u8 {
        /// Только чтение узла.
        Read = 1,
        /// Только запись узла.
        Write = 2,
        /// Чтение и запись узла.
        ReadWrite = 3,
    }
}

impl AccessMode {
    /// Допускает ли режим чтение.
    #[must_use]
    pub const fn readable(self) -> bool {
        matches!(self, Self::Read | Self::ReadWrite)
    }

    /// Допускает ли режим запись.
    #[must_use]
    pub const fn writable(self) -> bool {
        matches!(self, Self::Write | Self::ReadWrite)
    }
}

#[cfg(test)]
mod tests {
    use super::AccessMode;

    #[test]
    fn mode_capabilities_match_their_variants() {
        assert!(AccessMode::Read.readable() && !AccessMode::Read.writable());
        assert!(!AccessMode::Write.readable() && AccessMode::Write.writable());
        assert!(AccessMode::ReadWrite.readable() && AccessMode::ReadWrite.writable());
    }
}
