#![cfg_attr(not(test), no_std)]

#[cfg(feature = "alloc")]
pub mod buffered_writer;
pub mod byte_sink;
pub mod crlf;
pub mod mmio;
pub mod writer;
