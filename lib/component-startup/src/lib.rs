//! Сторона ребёнка стартовой сессии к родителю.
//!
//! Макрос [`component_manifest!`] объявляет требования и экспорты компонента и порождает поле
//! на каждое требование. Поле запрашивает сессию требования у родителя по вызову. Провайдер
//! выпускает сессии экспорта на [`SessionPort`], отвечает на заявки циклом [`serve_export`] и
//! принимает кадры порта сессий через [`SessionEvents`].
//!
//! Программа на Rust со стандартной библиотекой получает стартовый handle от
//! `components`.

#![no_std]

mod declare;
mod export;
mod import;
mod providers;
mod query;
mod request;

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
pub use request::{Found, SessionRequest, accept, all, next, open, session, watch};
pub use syscall::{Handle, ObjectKind};
