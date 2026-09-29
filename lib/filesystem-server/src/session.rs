use component_startup::{MintedSession, mint_session};
use filesystem_abi::FileSystemError;
use kernel_objects::Port;

use crate::{Shared, error::no_space};

pub(crate) fn open_session(shared: &Shared) -> Result<Port, FileSystemError> {
    let MintedSession { badge, port } =
        mint_session(&shared.service, &shared.next_badge).map_err(no_space)?;

    // Запись сессии создаётся до передачи её capability клиенту.
    let mut state = shared.state.lock();
    state.open_session(badge.get())?;

    Ok(port)
}
