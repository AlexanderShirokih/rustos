//! Байты, прочитанные у устройства.

use alloc::vec::Vec;

use ipc::{
    IntoWireValue, ReceivedResources, WireTyped, WireValue,
    schema::WireType,
    wire::{IpcError, MessageBuf},
};

/// Байты, прочитанные у устройства, в порядке приёма.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct TransferBuf(pub Vec<u8>);

impl<'a, R: ReceivedResources> WireValue<'a, R> for TransferBuf {
    type Owned = Self;

    fn from_field(data: &'a [u8], _frame: &mut R) -> Result<Self, IpcError> {
        Ok(Self(data.to_vec()))
    }
}

impl<R: ReceivedResources> IntoWireValue<R> for TransferBuf {
    type Read = Self;

    fn write_into(self, frame: &mut MessageBuf<R>, field_id: u8) -> Result<(), IpcError> {
        frame.write_field(field_id, &self.0)
    }
}

impl WireTyped for TransferBuf {
    const WIRE_TYPE: WireType = WireType::Bytes;
}
