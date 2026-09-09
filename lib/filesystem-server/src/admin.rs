//! Обслуживание startup-порта после стартовой транзакции: установление сессий владельцем
//! компонента вместе с областью, до которой сессия сужена.

use component_abi::{SESSION_SCOPE_MAX, SessionError, component_startup};
use filesystem_abi::{FileSystemError, PATH_MAX};
use ipc::wire::{Cap, Str};
use kernel_objects::{OwnedHandle, kind, settle, vend};

use crate::{Shared, service::mint_session};

const _: () = assert!(
    // Область приходит контрактом компонента, а путём её делает контракт файловой системы.
    SESSION_SCOPE_MAX <= PATH_MAX,
    "session scope exceeds the path bound"
);

pub(crate) struct Establishing<'a> {
    shared: &'a Shared,
    vended: Option<OwnedHandle<kind::Any>>,
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

impl component_startup::Service for Establishing<'_> {
    fn begin(&mut self, _log: Cap<kind::Port>, _export_count: u16, _slot_count: u16) {
        // Стартовая транзакция уже принята, и её кадры на рабочей фазе ответа не имеют.
    }

    fn export(&mut self, _capability: Cap<kind::Port>) {}

    fn slot(&mut self, _capability: Cap<kind::Any>) {}

    fn open_session(
        &mut self,
        export: u16,
        scope: Str<'_, SESSION_SCOPE_MAX>,
    ) -> Result<Cap<kind::Port>, SessionError> {
        // Экспорт у файловой системы один.
        if export != 0 {
            return Err(SessionError::Refused);
        }
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
