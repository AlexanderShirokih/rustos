use alloc::sync::{Arc, Weak};
use core::num::{NonZeroU16, NonZeroU32, NonZeroU64};

use syscall::Rights;

use super::{
    errors::IpcError,
    process::ProcessObject,
    rev_node::RevNode,
    rights::default_rights_for,
    target::{CapabilityTarget, HolderTicket, OperationHold},
};

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
    #[allow(clippy::cast_possible_truncation)]
    pub(super) const MAX_GENERATION: u16 = ((1u32 << (32 - Self::SLOT_BITS)) - 1) as u16;

    const GENERATION_SCALE: NonZeroU32 = match NonZeroU32::new(1 << Self::SLOT_BITS) {
        Some(scale) => scale,
        None => NonZeroU32::MIN,
    };

    pub(super) fn pack(generation: NonZeroU16, slot: u32) -> Self {
        debug_assert!(slot <= Self::MAX_SLOT_INDEX, "slot index overflow");
        let scaled = NonZeroU32::from(generation).saturating_mul(Self::GENERATION_SCALE);
        Self(scaled | (slot & Self::SLOT_MASK))
    }

    pub fn raw(self) -> NonZeroU32 {
        self.0
    }

    /// Восстанавливает [`HandleId`] из 32-битного значения.
    ///
    /// Корректность пары `(generation, slot)` валидируется при первом
    /// обращении к `HandleTable` - ошибочные id отвергаются как
    /// `IpcError::BadHandle`.
    pub const fn from_raw(raw: NonZeroU32) -> Self {
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
/// `Port` различаются значком. `0` означает "без значка". На приёме
/// сообщения ядро доставляет значок port-capability отправителя получателю.
pub struct Capability {
    pub(super) target: CapabilityTarget,
    rights: Rights,
    badge: u64,
    node: Arc<RevNode>,
    holder: Option<Arc<dyn HolderTicket>>,
    unheld: bool,
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
    /// Создаёт незаклеймённую capability (`badge == 0`) с правами `rights`.
    ///
    /// Capability становится корнем графа деривации. Потомки от [`duplicate`](Self::duplicate) и
    /// [`derive`](Self::derive) отзываются вместе с ней. Сам корень отзывается своим закрытием.
    pub fn new(target: CapabilityTarget, rights: Rights) -> Self {
        let node = RevNode::new_root();
        // Корневой узел ещё не виден другим потокам. Учёт вводится сразу с билетом,
        // поэтому отзыв не может опередить регистрацию хука.
        let holder = target.counted_holder_ticket(rights);
        if let Some(ticket) = &holder {
            let hook: Weak<dyn HolderTicket> = Arc::downgrade(ticket);
            node.register_hook(hook);
        }
        Self {
            target,
            rights,
            badge: 0,
            node,
            holder,
            unheld: false,
        }
    }

    /// Создаёт незаклеймённую capability на `process` с правами `rights`, не учитываемую в
    /// держателях процесса.
    ///
    /// Права и набор операций - как у [`new`](Self::new), но время жизни процесса такая
    /// capability не продлевает. Только `Process` допускает неучтённую capability.
    pub fn new_unheld_process(process: Arc<ProcessObject>, rights: Rights) -> Self {
        Self {
            target: CapabilityTarget::Process(process),
            rights,
            badge: 0,
            node: RevNode::new_root(),
            holder: None,
            unheld: true,
        }
    }

    /// Создаёт незаклеймённую capability со стартовыми правами объекта
    /// ([`default_rights_for`]).
    pub fn new_with_default_rights(target: impl Into<CapabilityTarget>) -> Self {
        let target = target.into();
        let rights = default_rights_for(&target);
        Self::new(target, rights)
    }

    fn armed_holder(
        target: &CapabilityTarget,
        rights: Rights,
        badge: Option<NonZeroU64>,
        node: &Arc<RevNode>,
    ) -> Result<Option<Arc<dyn HolderTicket>>, IpcError> {
        let ticket = match badge {
            Some(badge) => target.badged_holder_ticket(rights, badge),
            None => target.holder_ticket(rights),
        };
        let Some(ticket) = ticket else {
            return Ok(None);
        };
        // Revoke неучтённого билета учёт не трогает и потоков не будит: синхронный
        // отзыв на уже отозванном узле безопасен под локами вызывающего. Отзыв в
        // окне до активации её отвергает.
        let hook: Weak<dyn HolderTicket> = Arc::downgrade(&ticket);
        node.register_hook(hook);
        ticket.activate()?;
        Ok(Some(ticket))
    }

    pub(super) fn node(&self) -> &Arc<RevNode> {
        &self.node
    }

    pub fn rights(&self) -> Rights {
        self.rights
    }

    /// Значок (badge) этой capability. `0` означает "без значка".
    pub fn badge(&self) -> u64 {
        self.badge
    }

    pub fn target(&self) -> &CapabilityTarget {
        &self.target
    }

    pub(crate) fn hold_for_operation(&self) -> Option<OperationHold> {
        self.holder.as_ref()?.reacquire().map(OperationHold::new)
    }

    /// Создаёт копию capability с подмножеством прав и (опционально) значком.
    ///
    /// [`IpcError::AccessDenied`] - нет права [`Rights::DUPLICATE`] либо запрошенные права не
    /// подмножество существующих. [`IpcError::ResourceExhausted`] - объект не открывает учёт под
    /// новый значок. [`IpcError::Revoked`] - исходная capability отозвана, в том числе
    /// параллельно с дупликацией.
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
        let target = self.target.clone();

        let holder = if self.unheld {
            None
        } else {
            Self::armed_holder(&target, new_rights, NonZeroU64::new(badge), &node)?
        };

        Ok(Self {
            target,
            rights: new_rights,
            badge,
            node,
            holder,
            unheld: self.unheld,
        })
    }

    /// Создаёт capability на объект `target`, выведенный из объекта этой капы,
    /// узлом-потомком. Отзыв источника отзывает и производную.
    ///
    /// Права - стартовые для `target` ([`default_rights_for`]), суженные
    /// правами источника. Значок источника не наследуется: объект другой.
    /// Требует [`Rights::DUPLICATE`] у источника.
    pub fn derive(&self, target: impl Into<CapabilityTarget>) -> Result<Self, IpcError> {
        if !self.rights.contains(Rights::DUPLICATE) {
            return Err(IpcError::AccessDenied);
        }
        let target = target.into();
        let rights = default_rights_for(&target) & self.rights;
        let node = RevNode::new_child(&self.node).ok_or(IpcError::Revoked)?;
        let holder = Self::armed_holder(&target, rights, None, &node)?;
        Ok(Self {
            target,
            rights,
            badge: 0,
            node,
            holder,
            unheld: false,
        })
    }
}

impl Drop for Capability {
    fn drop(&mut self) {
        if let Some(holder) = &self.holder {
            holder.release();
        }
    }
}
