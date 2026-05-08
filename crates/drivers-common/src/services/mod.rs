/// Базовый маркерный трейт для capability-сервисов.
pub trait Service: Send + Sync {}

impl<T: ?Sized + Send + Sync> Service for T {}

pub mod console;
pub mod interrupts;
pub mod mmio;
pub mod scheduler;
pub mod timer;
pub mod user_image;
