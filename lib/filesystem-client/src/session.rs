//! Сессия файловой системы. Клиент контракта и отображённый регион её данных.

use filesystem_abi::file_system;
use ipc::{Timeout, wire::Cap};

use crate::{
    environment::{Environment, SharedRegion},
    error::Result,
};

const BUFFER_SIZE: u32 = 4096;

pub(crate) struct Session<E: Environment> {
    client: file_system::Client<E::Transport>,
    region: E::Region,
    capacity: u32,
}

impl<E: Environment> Session<E> {
    pub(crate) fn establish(environment: &E, session: Cap, wait: Timeout) -> Result<Self> {
        Self::over(environment, environment.connect(session)?, wait)
    }

    pub(crate) fn over(environment: &E, transport: E::Transport, wait: Timeout) -> Result<Self> {
        let client = file_system::Client::new(transport).with_wait(wait);
        let region = environment.map(client.acquire_buffer(BUFFER_SIZE)??)?;
        let capacity = u32::try_from(region.size())
            .unwrap_or(u32::MAX)
            .min(BUFFER_SIZE);

        Ok(Self {
            client,
            region,
            capacity,
        })
    }

    pub(crate) fn client(&self) -> &file_system::Client<E::Transport> {
        &self.client
    }

    pub(crate) fn region(&self) -> &E::Region {
        &self.region
    }

    pub(crate) fn capacity(&self) -> usize {
        usize::try_from(self.capacity).unwrap_or(usize::MAX)
    }

    pub(crate) fn wire_len(&self, len: usize) -> u32 {
        u32::try_from(len).unwrap_or(u32::MAX).min(self.capacity)
    }
}

#[cfg(test)]
mod tests {
    use filesystem_abi::{ENTRY_HEADER_SIZE, NAME_MAX};

    use super::BUFFER_SIZE;

    // Буфер короче одной записи каталога останавливает обход: `list` отвечает `BufferTooSmall`,
    // не продвинув позицию.
    #[test]
    fn the_buffer_holds_a_directory_entry_of_any_name() {
        let entry = ENTRY_HEADER_SIZE + NAME_MAX;
        let buffer = usize::try_from(BUFFER_SIZE).unwrap_or(0);

        assert!(buffer > entry);
    }
}
