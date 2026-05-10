//! `MemoryAuthority` KO: capability-токен на минтинг `Memory`-регионов.
//!
//! Самостоятельных данных не несёт — вся семантика выражена битами
//! [`Rights::CREATE_VIRTUAL`] / [`Rights::CREATE_PHYSICAL`] на конкретном
//! handle'е. Минтит только тот процесс, у кого есть handle с нужным правом;
//! сужение прав пересылаемой копии делается через `handle_duplicate`.

use alloc::sync::Arc;

#[derive(Debug)]
pub struct MemoryAuthority {
    _private: (),
}

impl MemoryAuthority {
    pub fn new() -> Arc<Self> {
        Arc::new(Self { _private: () })
    }
}
