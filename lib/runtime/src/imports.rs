//! Неявные импорты компонента. Родитель выдаёт их без объявления в манифесте.

use component_abi::{
    LOG_REQUIREMENT, STARTUP_REQUEST_TIMEOUT, SessionRequirement, component_startup,
};
use ipc::wire::Str;
use log_abi::log;
use syscall::{Handle, kind};

use crate::svc::PortTransport;

/// Запрашивает у родителя по стартовой сессии `parent` неявные импорты компонента и ставит их в
/// состояние процесса. Журнал получает вывод процесса с тегом `tag`. Отказ родителя оставляет
/// процесс без импорта. Handle стартовой сессии остаётся у вызывающего.
pub fn start_component(parent: Handle<kind::Port>, tag: &'static str) {
    if let Some(session) = import::<log::Protocol>(parent, LOG_REQUIREMENT) {
        crate::log::install(session, tag);
    }
}

fn import<P: ipc::Protocol>(parent: Handle<kind::Port>, name: &str) -> Option<Handle<kind::Port>> {
    let requirement = SessionRequirement {
        name: Str::new(name)?,
        protocol: Str::new(P::ID)?,
    };
    let requests = component_startup::Client::new(PortTransport::new(parent))
        .with_wait(STARTUP_REQUEST_TIMEOUT);
    Some(requests.session(requirement).ok()?.ok()?.into_raw())
}
