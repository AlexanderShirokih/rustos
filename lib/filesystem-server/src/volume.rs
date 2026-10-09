//! Тома процесса: состояние файловой системы на значение атрибута `label` заявки.

use alloc::vec::Vec;

use filesystem_abi::FileSystemError;

use crate::service::FileSystemState;

pub(crate) const VOLUME_LABEL: &[u8] = b"label";

pub(crate) const DEFAULT_VOLUME: usize = 0;

pub(crate) struct Volume {
    label: Vec<u8>,
    state: FileSystemState,
}

pub(crate) struct Volumes {
    default: FileSystemState,
    labelled: Vec<Volume>,
}

impl Volume {
    pub(crate) fn new(label: &[u8], state: FileSystemState) -> Result<Self, FileSystemError> {
        let mut owned = Vec::new();
        owned
            .try_reserve_exact(label.len())
            .map_err(|_| FileSystemError::NoSpace)?;
        owned.extend_from_slice(label);
        Ok(Self {
            label: owned,
            state,
        })
    }
}

impl Volumes {
    pub(crate) fn new(default: FileSystemState) -> Self {
        Self {
            default,
            labelled: Vec::new(),
        }
    }

    pub(crate) fn find(&self, label: &[u8]) -> Option<usize> {
        if label.is_empty() {
            return Some(DEFAULT_VOLUME);
        }
        self.labelled
            .iter()
            .position(|volume| volume.label.as_slice() == label)
            .map(|index| index.saturating_add(1))
    }

    pub(crate) fn insert(&mut self, volume: Volume) -> Result<usize, FileSystemError> {
        // Том с той же меткой, добавленный между поиском и вставкой, остаётся, а новый
        // отбрасывается.
        if let Some(index) = self.find(&volume.label) {
            return Ok(index);
        }

        self.labelled
            .try_reserve(1)
            .map_err(|_| FileSystemError::NoSpace)?;
        self.labelled.push(volume);
        Ok(self.labelled.len())
    }

    pub(crate) fn of(&self, badge: u64) -> usize {
        self.labelled
            .iter()
            .position(|volume| volume.state.serves(badge))
            .map_or(DEFAULT_VOLUME, |index| index.saturating_add(1))
    }

    pub(crate) fn state(&mut self, volume: usize) -> &mut FileSystemState {
        match volume
            .checked_sub(1)
            .and_then(|index| self.labelled.get_mut(index))
        {
            Some(volume) => &mut volume.state,
            None => &mut self.default,
        }
    }

    pub(crate) fn remove_session(&mut self, badge: u64) {
        self.default.remove_session(badge);

        for volume in &mut self.labelled {
            volume.state.remove_session(badge);
        }
    }
}
