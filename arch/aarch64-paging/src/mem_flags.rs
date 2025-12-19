#[repr(transparent)]
#[derive(Copy, Clone, Debug, Eq, PartialEq)]
pub struct MemFlags(u64);

#[derive(Copy, Clone)]
pub enum Shareability {
    None,
    Outer,
    Inner,
}

#[derive(Copy, Clone)]
pub enum Access {
    KernelRW,
    KernelRO,
    UserRW,
    UserRO,
}

impl MemFlags {
    pub const fn new() -> Self {
        Self(0)
    }

    pub const fn from_bits(bits: u64) -> Self {
        Self(bits)
    }

    pub const fn af(self) -> Self {
        Self(self.0 | (1 << 10))
    }

    pub const fn sh(self, sh: Shareability) -> Self {
        let bits = match sh {
            Shareability::None => 0b00,
            Shareability::Outer => 0b10,
            Shareability::Inner => 0b11,
        };
        Self((self.0 & !(0b11 << 8)) | ((bits as u64) << 8))
    }

    /// Stage-1 AttrIndx[2:0] (MAIR index)
    pub const fn attr_index(self, idx: u8) -> Self {
        Self((self.0 & !(0b111 << 2)) | (((idx as u64) & 0b111) << 2))
    }

    pub const fn ap(self, ap: Access) -> Self {
        // AP[2:1] в stage-1: 00=EL1 RW, 01=EL1 RO, 10=EL0/1 RW, 11=EL0/1 RO
        let bits = match ap {
            Access::KernelRW => 0b00,
            Access::KernelRO => 0b01,
            Access::UserRW => 0b10,
            Access::UserRO => 0b11,
        };
        Self((self.0 & !(0b11 << 6)) | ((bits as u64) << 6))
    }

    pub const fn uxn(self, on: bool) -> Self {
        if on { Self(self.0 | (1 << 54)) } else { self }
    }

    pub const fn pxn(self, on: bool) -> Self {
        if on { Self(self.0 | (1 << 53)) } else { self }
    }

    pub const fn bits(self) -> u64 {
        self.0
    }
}
