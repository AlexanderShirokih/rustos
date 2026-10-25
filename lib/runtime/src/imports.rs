//! Неявные импорты компонента. Родитель выдаёт их без объявления в манифесте.

use component_abi::{
    Attributes, LOG_REQUIREMENT, STARTUP_REQUEST_TIMEOUT, SessionRequirement, component_startup,
};
use log_abi::log;
use syscall::{Handle, kind};

use crate::svc::{IssuedHandle, PortTransport};

/// Запрашивает у родителя по стартовой сессии `parent` неявные импорты компонента и ставит их в
/// состояние процесса. Отказ родителя оставляет процесс без импорта. Handle стартовой сессии
/// остаётся у вызывающего.
pub fn start_component(parent: Handle<kind::Port>) {
    if let Some(session) = import::<log::Protocol>(parent, LOG_REQUIREMENT) {
        crate::log::install(session);
    }
}

fn import<P: ipc::Protocol>(
    parent: Handle<kind::Port>,
    name: &str,
) -> Option<IssuedHandle<kind::Port>> {
    let requirement = SessionRequirement {
        name,
        protocol: P::ID,
    };

    let requests = component_startup::Client::new(PortTransport::new(parent))
        .with_wait(STARTUP_REQUEST_TIMEOUT);
    requests
        .session(requirement, Attributes::empty().as_bytes())
        .wait()
        .ok()
}
