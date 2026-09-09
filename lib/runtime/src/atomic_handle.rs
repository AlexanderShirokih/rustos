//! Восстановление хэндла из атомарного raw-поля состояния процесса.

use core::num::NonZeroU32;

use syscall::{Handle, ObjectKind};

pub(crate) fn restore<K: ObjectKind>(raw: u32) -> Option<Handle<K>> {
    NonZeroU32::new(raw).map(Handle::from_abi)
}
