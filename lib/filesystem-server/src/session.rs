use component_startup::{MintedSession, mint_session};
use filesystem_abi::FileSystemError;
use kernel_objects::Port;

use crate::{error::no_space, server::Shared};

pub(crate) fn open_session(shared: &Shared, volume: usize) -> Result<Port, FileSystemError> {
    let MintedSession { badge, port } =
        mint_session(&shared.service, &shared.next_badge).map_err(no_space)?;

    // Запись сессии создаётся до передачи её capability клиенту.
    let mut volumes = shared.volumes.lock();
    volumes.state(volume).open_session(badge.get())?;

    Ok(port)
}
