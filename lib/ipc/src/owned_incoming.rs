use alloc::{boxed::Box, vec::Vec};

use crate::{Incoming, wire::IpcError};

/// Принятый кадр с собственными байтами и ресурсами доставки.
///
/// Незавершённый обработчик удерживает этот кадр независимо от буфера приёма. Байты лежат
/// в куче по длине кадра. Разрушение кадра закрывает невзятые capability и невзятый канал
/// ответа.
pub struct OwnedIncoming<R> {
    bytes: Box<[u8]>,
    /// Ресурсы доставки: capability кадра и канал ответа встречного вызова.
    pub resources: R,
}

impl<R> OwnedIncoming<R> {
    /// Копирует байты и переносит ресурсы кадра. Нехватка памяти даёт `ResourceExhausted`
    /// и закрывает ресурсы.
    pub fn new(incoming: Incoming<'_, R>) -> Result<Self, IpcError> {
        let mut bytes = Vec::new();
        bytes
            .try_reserve_exact(incoming.bytes.len())
            .map_err(|_| IpcError::ResourceExhausted)?;
        bytes.extend_from_slice(incoming.bytes);

        Ok(Self {
            bytes: bytes.into_boxed_slice(),
            resources: incoming.resources,
        })
    }

    /// Байты кадра.
    pub fn bytes(&self) -> &[u8] {
        &self.bytes
    }

    /// Байты кадра и ресурсы для разбора с переносом capability в параметры операции.
    #[doc(hidden)]
    pub fn as_incoming(&mut self) -> Incoming<'_, &mut R> {
        Incoming {
            bytes: &self.bytes,
            resources: &mut self.resources,
        }
    }
}
