use alloc::sync::Arc;
use core::num::NonZeroU32;

use super::{errors::IpcError, koid::Koid, object::KObject, rev_node::RevNode, rights::Rights};

/// Публичный идентификатор записи в `HandleTable`, используемый процессами для IPC.
///
/// Внутреннее представление - упакованная пара `(generation, slot)`:
/// 16 старших бит - generation (1..=65535), 16 младших - индекс слота
/// (0..=2^16-1).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct HandleId(NonZeroU32);

impl HandleId {
    pub(super) const SLOT_BITS: u32 = 16;
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

    /// Восстанавливает [`HandleId`] из 32-битного значения.
    ///
    /// Корректность пары `(generation, slot)` валидируется при первом
    /// обращении к `HandleTable` - ошибочные id отвергаются как
    /// `IpcError::BadHandle`.
    pub fn from_raw(raw: NonZeroU32) -> Self {
        Self(raw)
    }

    pub(super) fn generation(self) -> u16 {
        (self.0.get() >> Self::SLOT_BITS) as u16
    }

    pub(super) fn slot(self) -> u32 {
        self.0.get() & Self::SLOT_MASK
    }
}

/// Запись в [`HandleTable`](super::HandleTable): kernel-объект,
/// права, с которыми этот handle может быть использован, и значок (badge).
///
/// `badge` - свойство ХЕНДЛА, не объекта: разные хендлы на один и тот же
/// `Port` несут разные значки. `0` означает «без значка». На приёме
/// сообщения ядро доставляет значок port-хендла отправителя получателю
/// (см. RFC-0001, badge как идентификация клиента, аналог seL4-badges).
pub struct Handle {
    pub(super) object: KObject,
    rights: Rights,
    badge: u64,
    node: Arc<RevNode>,
}

impl core::fmt::Debug for Handle {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        f.debug_struct("Handle")
            .field("koid", &self.object.koid())
            .field("rights", &self.rights)
            .field("badge", &self.badge)
            .finish_non_exhaustive()
    }
}

impl Handle {
    /// Создаёт незаклеймённый handle (`badge == 0`).
    pub fn new(object: KObject, rights: Rights) -> Self {
        Self {
            object,
            rights,
            badge: 0,
            node: RevNode::new_root(),
        }
    }

    /// Создаёт handle с заданным значком `badge`.
    pub fn new_with_badge(object: KObject, rights: Rights, badge: u64) -> Self {
        Self {
            object,
            rights,
            badge,
            node: RevNode::new_root(),
        }
    }

    /// Узел графа деривации этой капы.
    pub(super) fn node(&self) -> &Arc<RevNode> {
        &self.node
    }

    pub fn rights(&self) -> Rights {
        self.rights
    }

    /// Значок (badge) этого хендла; `0` означает «без значка».
    pub fn badge(&self) -> u64 {
        self.badge
    }

    pub fn koid(&self) -> Koid {
        self.object.koid()
    }

    pub fn object(&self) -> &KObject {
        &self.object
    }

    /// Создаёт копию handle'а с подмножеством прав и (опционально) значком.
    ///
    /// Возвращает [`IpcError::AccessDenied`], если у исходного handle'а нет
    /// права [`Rights::DUPLICATE`] либо запрошенный набор прав не является
    /// подмножеством существующего.
    pub fn duplicate(&self, new_rights: Rights, new_badge: u64) -> Result<Self, IpcError> {
        if !self.rights.contains(Rights::DUPLICATE) {
            return Err(IpcError::AccessDenied);
        }
        if !new_rights.is_subset_of(self.rights) {
            return Err(IpcError::AccessDenied);
        }
        let badge = if self.badge != 0 {
            if new_badge != 0 {
                return Err(IpcError::BadHandle);
            }
            self.badge
        } else {
            new_badge
        };
        Ok(Self {
            object: self.object.clone(),
            rights: new_rights,
            badge,
            // Производная капа - дочерний узел деривации: закрытие источника
            // (предка) лениво отзовёт эту копию.
            node: RevNode::new_child(&self.node),
        })
    }
}
