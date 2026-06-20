use core::ops::{BitAnd, BitAndAssign, BitOr, BitOrAssign, Not};

/// Набор прав, ассоциированных с конкретным [`Handle`](super::Handle).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Hash)]
#[repr(transparent)]
pub struct Rights(u32);

impl Rights {
    pub const NONE: Self = Self(0);
    pub const DUPLICATE: Self = Self(1 << 0);
    pub const TRANSFER: Self = Self(1 << 1);
    pub const READ: Self = Self(1 << 2);
    pub const WRITE: Self = Self(1 << 3);
    pub const EXECUTE: Self = Self(1 << 4);

    const ALL_BITS: u32 =
        Self::DUPLICATE.0 | Self::TRANSFER.0 | Self::READ.0 | Self::WRITE.0 | Self::EXECUTE.0;

    pub const fn empty() -> Self {
        Self(0)
    }

    pub const fn all() -> Self {
        Self(Self::ALL_BITS)
    }

    pub const fn bits(self) -> u32 {
        self.0
    }

    /// Принимает произвольный набор битов, отбрасывая неизвестные позиции.
    pub const fn from_bits_truncate(bits: u32) -> Self {
        Self(bits & Self::ALL_BITS)
    }

    pub const fn is_empty(self) -> bool {
        self.0 == 0
    }

    /// `true`, если `self` содержит все биты из `other`.
    pub const fn contains(self, other: Self) -> bool {
        (self.0 & other.0) == other.0
    }

    /// `true`, если все биты `self` уже присутствуют в `other`.
    pub const fn is_subset_of(self, other: Self) -> bool {
        (self.0 & other.0) == self.0
    }

    /// Стартовый набор прав для свежесозданного KO данного типа.
    pub fn defaults_for(obj: &super::object::KObject) -> Self {
        use super::object::KObject;

        match obj {
            // Signal сигналуем пользователем; Process/Thread термнинацию
            // поднимает только ядро (через bound-Signal), но WRITE на самом
            // объекте оставлен под terminate-op - наборы прав совпадают.
            KObject::Signal(_) | KObject::Process(_) | KObject::Thread(_) => {
                Self(Self::READ.0 | Self::WRITE.0 | Self::DUPLICATE.0 | Self::TRANSFER.0)
            }
            
            KObject::Memory(region) => {
                // База — только передаваемость/дублируемость; конкретный доступ
                // (READ/WRITE/EXECUTE) определяется access-маской региона, чтобы
                // read-only регион не получал WRITE по умолчанию.
                let mut bits = Self::DUPLICATE.0 | Self::TRANSFER.0;
                let access = region.access_mask();
                if access.allows(memory::AccessMask::R) {
                    bits |= Self::READ.0;
                }
                if access.allows(memory::AccessMask::W) {
                    bits |= Self::WRITE.0;
                }
                if access.allows(memory::AccessMask::X) {
                    bits |= Self::EXECUTE.0;
                }
                Self(bits)
            }
            
            KObject::Resource(_) => {
                Self(Self::DUPLICATE.0 | Self::TRANSFER.0 | Self::READ.0 | Self::WRITE.0)
            }
            
            // Port: send гейтится WRITE, recv - READ (как channel
            // write/read); делегируется и дублируется.
            KObject::Port(_) => {
                Self(Self::READ.0 | Self::WRITE.0 | Self::TRANSFER.0 | Self::DUPLICATE.0)
            }
            
            // Reply: WRITE гейтит сам reply; TRANSFER даёт делегировать
            // ответ другому серверу. Не дублируется.
            KObject::Reply(_) => Self(Self::WRITE.0 | Self::TRANSFER.0),
        }
    }
}

impl BitOr for Rights {
    type Output = Self;
    fn bitor(self, rhs: Self) -> Self {
        Self(self.0 | rhs.0)
    }
}

impl BitOrAssign for Rights {
    fn bitor_assign(&mut self, rhs: Self) {
        self.0 |= rhs.0;
    }
}

impl BitAnd for Rights {
    type Output = Self;
    fn bitand(self, rhs: Self) -> Self {
        Self(self.0 & rhs.0)
    }
}

impl BitAndAssign for Rights {
    fn bitand_assign(&mut self, rhs: Self) {
        self.0 &= rhs.0;
    }
}

impl Not for Rights {
    type Output = Self;
    fn not(self) -> Self {
        Self(!self.0 & Self::ALL_BITS)
    }
}

#[cfg(test)]
mod tests {
    use super::{
        super::{object::KObject, process::ProcessObject, thread::ThreadObject},
        *,
    };

    #[test]
    fn defaults_for_process_grants_write_and_read() {
        let ko = KObject::Process(ProcessObject::new());
        let r = Rights::defaults_for(&ko);
        assert!(r.contains(Rights::WRITE));
        assert!(r.contains(Rights::READ));
        assert!(r.contains(Rights::DUPLICATE));
        assert!(r.contains(Rights::TRANSFER));
    }

    #[test]
    fn defaults_for_process_omits_execute() {
        let ko = KObject::Process(ProcessObject::new());
        let r = Rights::defaults_for(&ko);
        assert!(!r.contains(Rights::EXECUTE));
    }

    #[test]
    fn defaults_for_thread_grants_write_and_read() {
        let ko = KObject::Thread(ThreadObject::new());
        let r = Rights::defaults_for(&ko);
        assert!(r.contains(Rights::WRITE));
        assert!(r.contains(Rights::READ));
        assert!(r.contains(Rights::DUPLICATE));
        assert!(r.contains(Rights::TRANSFER));
    }

    #[test]
    fn defaults_for_thread_omits_execute() {
        let ko = KObject::Thread(ThreadObject::new());
        let r = Rights::defaults_for(&ko);
        assert!(!r.contains(Rights::EXECUTE));
    }
}
