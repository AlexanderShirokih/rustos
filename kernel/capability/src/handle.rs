use alloc::sync::Arc;
use core::num::NonZeroU32;

use super::{errors::IpcError, rev_node::RevNode, rights::Rights, target::CapabilityTarget};

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

/// Запись в [`HandleTable`](super::HandleTable): capability target,
/// права, с которыми эта capability может быть использована, и значок (badge).
///
/// `badge` - свойство capability: разные capability на один и тот же
/// `Port` несут разные значки. `0` означает "без значка". На приёме
/// сообщения ядро доставляет значок port-capability отправителя получателю.
pub struct Capability {
    pub(super) target: CapabilityTarget,
    rights: Rights,
    badge: u64,
    node: Arc<RevNode>,
}

impl core::fmt::Debug for Capability {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        f.debug_struct("Capability")
            .field("rights", &self.rights)
            .field("badge", &self.badge)
            .finish_non_exhaustive()
    }
}

impl Capability {
    /// Создаёт незаклеймённую capability (`badge == 0`).
    pub fn new(target: CapabilityTarget, rights: Rights) -> Self {
        Self {
            target,
            rights,
            badge: 0,
            node: RevNode::new_root(),
        }
    }

    /// Создаёт capability с заданным значком `badge`.
    pub fn new_with_badge(target: CapabilityTarget, rights: Rights, badge: u64) -> Self {
        Self {
            target,
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

    /// Значок (badge) этой capability; `0` означает «без значка».
    pub fn badge(&self) -> u64 {
        self.badge
    }

    pub fn target(&self) -> &CapabilityTarget {
        &self.target
    }

    /// Создаёт копию capability с подмножеством прав и (опционально) значком.
    ///
    /// Возвращает [`IpcError::AccessDenied`], если у исходной capability нет
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
        let node = RevNode::new_child(&self.node).ok_or(IpcError::Revoked)?;
        Ok(Self {
            target: self.target.clone(),
            rights: new_rights,
            badge,
            node,
        })
    }
}
