use alloc::sync::Arc;

/// Общее адресное пространство ядра
#[derive(Debug)]
pub struct AddressSpace;

impl AddressSpace {
    pub fn shared_kernel() -> Arc<Self> {
        Arc::new(Self)
    }
}
