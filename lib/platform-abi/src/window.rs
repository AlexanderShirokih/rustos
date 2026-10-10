use kernel_objects::MemoryRegion;
use syscall::kind;

ipc::wire_value! {
    /// Окно регистров устройства: регион, смещение окна от его начала и размер окна.
    #[derive(Debug, Clone, Copy, PartialEq, Eq)]
    pub struct MmioWindow<M = ipc::wire::Cap<kind::MemoryRegion>> {
        /// Регион, который целиком содержит окно.
        pub region: M,
        /// Смещение первого регистра окна от начала региона.
        pub offset: u64,
        /// Размер окна из описания устройства.
        pub size: u64,
    }
}

impl<M: Into<MemoryRegion>> From<MmioWindow<M>> for kernel_objects::MmioWindow {
    fn from(window: MmioWindow<M>) -> Self {
        Self {
            region: window.region.into(),
            offset: window.offset,
            size: window.size,
        }
    }
}
