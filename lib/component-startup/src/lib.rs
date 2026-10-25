//! Сторона ребёнка стартовой сессии к родителю.
//!
//! Макрос [`component_manifest!`] объявляет требования и экспорты компонента и порождает поле
//! на каждое требование. Поле запрашивает сессию требования у родителя по вызову. Провайдер
//! выпускает сессии экспорта на [`SessionPort`]. [`serve_component`] обслуживает заявки и
//! вызовы сессий на одном executor.
//!
//! Программа на Rust со стандартной библиотекой получает стартовый handle от
//! `components`.

#![no_std]

extern crate alloc;

mod declare;
mod export;
mod import;
mod providers;
mod query;
mod request;
mod serve;

// Раскрытие `component_manifest!` вызывает макрос записи манифеста по этому пути, и
// объявляющая программа не объявляет своей зависимости на `component-abi`.
#[doc(hidden)]
pub use component_abi;
pub use component_abi::{
    AcceptError, Attribute, Attributes, LOG_REQUIREMENT, STARTUP_REQUEST_TIMEOUT, SessionError,
    component_startup,
};
#[doc(hidden)]
pub use declare::start_parent;
pub use declare::{Requirements, StartupError, StartupFailure};
pub use export::{
    ExportStopped, MintedSession, SessionEvent, SessionEvents, SessionPort, serve_export,
    whole_export,
};
pub use import::Import;
pub use ipc::wire::IpcError;
pub use kernel_objects::{OwnedHandle, Port, PortTransport, Session, kind};
pub use providers::{ProviderSessions, ProviderWatch, Providers};
pub use query::{AnyOf, Query};
pub use request::{
    AsyncStartup, Found, SessionRequest, accept, all, next, open, session, session_async, watch,
};
pub use serve::{ServeStopped, serve_calls, serve_component};
pub use syscall::{Handle, ObjectKind};
