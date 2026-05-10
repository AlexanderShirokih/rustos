//! `PhysicalResource` KO: capability-токен на минтинг `Memory`-регионов
//! поверх фиксированного физического адреса (MMIO, DMA-буферы).
//!
//! Самостоятельных данных не несёт — семантика выражена битом
//! [`Rights::MINT`](super::Rights::MINT) на handle'е. Сужение прав
//! пересылаемой копии делается через `handle_duplicate`.

use alloc::sync::Arc;

#[derive(Debug)]
pub struct PhysicalResource {
    _private: (),
}

impl PhysicalResource {
    pub fn new() -> Arc<Self> {
        Arc::new(Self { _private: () })
    }
}
