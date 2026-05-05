//! Контракт arch-зависимого слоя: остановка эмулятора.
//!
//! Backend регистрируется однократно через
//! [`runner::install_backend`](crate::runner::install_backend); ядро
//! харнесса использует его через [`runner::with_backend`].

pub trait Backend: Send + Sync {
    fn exit(&self, code: u32) -> !;
}
