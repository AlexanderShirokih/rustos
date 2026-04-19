use alloc::sync::Arc;

/// Общее адресное пространство ядра; user-space будет добавлен позже.
#[derive(Debug)]
pub struct AddressSpace;

impl AddressSpace {
    pub fn shared_kernel() -> Arc<Self> {
        Arc::new(Self)
    }
}
