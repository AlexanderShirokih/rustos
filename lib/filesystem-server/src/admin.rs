//! Обслуживание control-порта: установление сессий владельцем компонента вместе с областью,
//! до которой сессия сужена.

use component_abi::{SESSION_SCOPE_MAX, SessionError, component_sessions};
use filesystem_abi::{FileSystemError, PATH_MAX};
use ipc::wire::{Cap, Str};
use kernel_objects::OwnedHandle;

use crate::{
    Shared,
    service::{mint_session, settle, vend},
};

const _: () = assert!(
    // Область приходит контрактом компонента, а путём её делает контракт файловой системы.
    SESSION_SCOPE_MAX <= PATH_MAX,
    "session scope exceeds the path bound"
);

pub(crate) struct Establishing<'a> {
    shared: &'a Shared,
    vended: Option<OwnedHandle>,
}

impl<'a> Establishing<'a> {
    pub(crate) fn new(shared: &'a Shared) -> Self {
        Self {
            shared,
            vended: None,
        }
    }

    pub(crate) fn end(&mut self, delivered: bool) {
        settle(&mut self.vended, delivered);
    }
}

impl component_sessions::Service for Establishing<'_> {
    fn open_session(&mut self, scope: Str<'_, SESSION_SCOPE_MAX>) -> Result<Cap, SessionError> {
        let (badge, session) =
            mint_session(&self.shared.service, &self.shared.next_badge).map_err(refused)?;
        // Сессия создаётся до того, как её капа ушла клиенту. Кадр по ней застанет её созданной.
        let mut state = self.shared.state.lock();
        let opened = match scope.as_str() {
            "" => state.open_session(badge.get()),
            path => state.open_session_at(badge.get(), path),
        };
        drop(state);
        opened.map_err(refused)?;
        Ok(vend(&mut self.vended, session))
    }
}

fn refused(error: FileSystemError) -> SessionError {
    match error {
        FileSystemError::NoSpace => SessionError::NoSpace,
        _ => SessionError::Refused,
    }
}
