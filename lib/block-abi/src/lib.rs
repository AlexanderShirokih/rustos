//! Контракт [`block_device`] блочного устройства: сведения о диске и чтение и запись его блоков
//! через буфер данных сессии.
//!
//! Блок - единица адреса и длины запроса. Сессия открыта к одному диску. Данные запроса идут
//! через буфер, который сессия получает операцией `acquire_buffer`, кадр содержит номер первого
//! блока и число блоков.

#![cfg_attr(not(test), no_std)]

mod error;
mod info;
mod protocol;

pub use error::BlockError;
pub use info::BlockInfo;
pub use protocol::{SESSION_WAIT, block_device};
