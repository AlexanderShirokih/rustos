//! Тома процесса: у каждого набора атрибутов заявки своё состояние файловой системы.

use alloc::vec::Vec;

use component_abi::{AttributeSet, Attributes};
use filesystem_abi::FileSystemError;

use crate::service::FileSystemState;

pub(crate) struct Volume {
    attributes: AttributeSet,
    state: FileSystemState,
}

#[derive(Default)]
pub(crate) struct Volumes {
    volumes: Vec<Volume>,
}

impl Volume {
    pub(crate) fn new(attributes: Attributes<'_>, state: FileSystemState) -> Self {
        Self {
            attributes: AttributeSet::new(attributes),
            state,
        }
    }
}

impl Volumes {
    pub(crate) fn find(&self, attributes: Attributes<'_>) -> Option<usize> {
        self.volumes
            .iter()
            .position(|volume| volume.attributes.as_attributes().same_set(attributes))
    }

    pub(crate) fn insert(&mut self, volume: Volume) -> Result<usize, FileSystemError> {
        // Том того же набора, добавленный между поиском и вставкой, остаётся, а новый
        // отбрасывается.
        if let Some(index) = self.find(volume.attributes.as_attributes()) {
            return Ok(index);
        }

        self.volumes
            .try_reserve(1)
            .map_err(|_| FileSystemError::NoSpace)?;
        self.volumes.push(volume);
        Ok(self.volumes.len().saturating_sub(1))
    }

    pub(crate) fn of(&self, badge: u64) -> usize {
        self.volumes
            .iter()
            .position(|volume| volume.state.serves(badge))
            .unwrap_or_default()
    }

    pub(crate) fn state(&mut self, volume: usize) -> Option<&mut FileSystemState> {
        self.volumes.get_mut(volume).map(|volume| &mut volume.state)
    }

    pub(crate) fn remove_session(&mut self, badge: u64) {
        for volume in &mut self.volumes {
            volume.state.remove_session(badge);
        }
    }
}
