/// Базовый маркерный трейт для capability-сервисов.
pub trait Service: Send + Sync {}

impl<T: ?Sized + Send + Sync> Service for T {}

pub mod interrupts;
pub mod mmio;
pub mod timer;
