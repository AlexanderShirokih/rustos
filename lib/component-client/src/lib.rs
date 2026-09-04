//! Приём стартовой транзакции компонента программой.
//!
//! Крейт состояния процесса не держит. Он принимает уже выданный стартовый порт, переводит
//! capability транзакции во владеющие хэндлы и отдаёт их программе одним объявлением. Автомат
//! кадров и запись манифеста остаются за `component-abi`. Источник стартового порта у
//! программы со стандартной библиотекой лежит в `component-client-std`.

#![no_std]

mod declare;
mod receive;

// Раскрытие `component_manifest!` вызывает макрос записи манифеста по этому пути, и
// объявляющая программа не объявляет своей зависимости на `component-abi`.
#[doc(hidden)]
pub use component_abi;
pub use kernel_objects::OwnedHandle;
pub use receive::{ReceivedStartup, StartupError, StartupSet, receive_set};
