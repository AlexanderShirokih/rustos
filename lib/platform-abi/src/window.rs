use syscall::kind;

ipc::wire_value! {
    /// Окно регистров устройства: регион и смещение первого регистра от его начала.
    #[derive(Debug, Clone, Copy, PartialEq, Eq)]
    pub struct MmioWindow<M = ipc::wire::Cap<kind::MemoryRegion>> {
        /// Регион, который целиком содержит регистры устройства.
        pub region: M,
        /// Смещение первого регистра устройства от начала региона.
        pub offset: u64,
    }
}
