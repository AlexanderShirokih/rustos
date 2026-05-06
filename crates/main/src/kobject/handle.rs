use core::num::NonZeroU32;

use super::{errors::IpcError, object::KObject, koid::Koid, rights::Rights};

/// Публичный идентификатор записи в `HandleTable`, используемый процессами для IPC.
///
/// Внутреннее представление - упакованная пара `(generation, slot)`:
/// 12 старших бит - generation (1..=4095), 20 младших - индекс слота
/// (0..=2^20-1).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct HandleId(NonZeroU32);

impl HandleId {
    pub(super) const SLOT_BITS: u32 = 20;
    pub(super) const SLOT_MASK: u32 = (1 << Self::SLOT_BITS) - 1;
    pub(super) const MAX_SLOT_INDEX: u32 = Self::SLOT_MASK;
    pub(super) const MAX_GENERATION: u16 = ((1u32 << (32 - Self::SLOT_BITS)) - 1) as u16;

    /// Упаковывает `(generation, slot)` в идентификатор.
    pub(super) fn pack(generation: u16, slot: u32) -> Self {
        debug_assert!(generation >= 1, "generation must be >= 1");
        debug_assert!(
            u32::from(generation) <= u32::from(Self::MAX_GENERATION),
            "generation overflow"
        );
        debug_assert!(slot <= Self::MAX_SLOT_INDEX, "slot index overflow");
        let raw = (u32::from(generation) << Self::SLOT_BITS) | slot;
        Self(NonZeroU32::new(raw).expect("generation >= 1 guarantees non-zero"))
    }

    pub fn raw(self) -> NonZeroU32 {
        self.0
    }

    pub(super) fn generation(self) -> u16 {
        (self.0.get() >> Self::SLOT_BITS) as u16
    }

    pub(super) fn slot(self) -> u32 {
        self.0.get() & Self::SLOT_MASK
    }
}

/// Запись в [`HandleTable`](super::HandleTable): kernel-объект и
/// права, с которыми этот handle может быть использован.
pub struct Handle {
    pub(super) object: KObject,
    rights: Rights,
}

impl core::fmt::Debug for Handle {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        f.debug_struct("Handle")
            .field("koid", &self.object.koid())
            .field("type_tag", &self.object.type_tag())
            .field("rights", &self.rights)
            .finish()
    }
}

impl Handle {
    pub fn new(object: KObject, rights: Rights) -> Self {
        Self { object, rights }
    }

    pub fn rights(&self) -> Rights {
        self.rights
    }

    pub fn koid(&self) -> Koid {
        self.object.koid()
    }

    pub fn object(&self) -> &KObject {
        &self.object
    }

    /// Создаёт копию handle'а с подмножеством прав.
    ///
    /// Возвращает [`IpcError::AccessDenied`], если у исходного handle'а нет
    /// права [`Rights::DUPLICATE`] либо запрошенный набор прав не является
    /// подмножеством существующего.
    pub fn duplicate(&self, new_rights: Rights) -> Result<Self, IpcError> {
        if !self.rights.contains(Rights::DUPLICATE) {
            return Err(IpcError::AccessDenied);
        }
        if !new_rights.is_subset_of(self.rights) {
            return Err(IpcError::AccessDenied);
        }
        Ok(Self {
            object: self.object.clone(),
            rights: new_rights,
        })
    }
}
