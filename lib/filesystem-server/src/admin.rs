//! Обслуживание control-порта: установление сессий владельцем компонента.

use filesystem_abi::{FileSystemError, file_system_admin};
use ipc::wire::Cap;
use runtime::OwnedHandle;

use crate::{
    Shared,
    service::{mint_session, settle, vend},
};

pub(crate) struct Binding<'a> {
    shared: &'a Shared,
    vended: Option<OwnedHandle>,
}

impl<'a> Binding<'a> {
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

impl file_system_admin::Service for Binding<'_> {
    fn bind(&mut self) -> Result<Cap, FileSystemError> {
        let (badge, session) = mint_session(&self.shared.service, &self.shared.next_badge)?;
        // Сессия заводится до того, как её капа ушла клиенту: кадр по ней застанет её заведённой.
        self.shared.state.lock().open_session(badge.get())?;
        Ok(vend(&mut self.vended, session))
    }
}
