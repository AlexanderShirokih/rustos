//! Сторона ребёнка стартовой сессии к родителю.
//!
//! Макрос [`component_manifest!`] объявляет требования и экспорты компонента и порождает их
//! запрос у родителя. Провайдер обслуживает экспорт циклом [`serve_export`].
//!
//! Программа на Rust со стандартной библиотекой получает стартовый handle от
//! `component-startup-std`.

#![no_std]

mod declare;
mod export;
mod request;

// Раскрытие `component_manifest!` вызывает макрос записи манифеста по этому пути, и
// объявляющая программа не объявляет своей зависимости на `component-abi`.
#[doc(hidden)]
pub use component_abi;
pub use component_abi::{
    AcceptError, LOG_REQUIREMENT, ResourceError, SessionError, component_startup,
};
pub use declare::{STARTUP_REQUEST_TIMEOUT, Startup, StartupError, StartupFailure};
#[doc(hidden)]
pub use declare::{start_log, start_parent, start_resource, start_session};
pub use export::{ExportError, MintedSession, mint_session, serve_export, whole_export};
pub use ipc::wire::IpcError;
pub use kernel_objects::{IrqLine, MemoryRegion, OwnedHandle, Port, PortTransport, Session, kind};
pub use request::{SessionRequest, accept, session};
pub use syscall::{Handle, ObjectKind};
