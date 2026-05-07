use core::fmt;

/// Значение регистра общего назначения (x0-x30).
#[repr(transparent)]
#[derive(Clone, Copy, PartialEq, Eq)]
pub struct GpReg(u64);

impl GpReg {
    pub const fn from_u64(value: u64) -> Self {
        Self(value)
    }

    pub const fn get(self) -> u64 {
        self.0
    }
}

impl From<u64> for GpReg {
    fn from(value: u64) -> Self {
        Self(value)
    }
}

impl From<GpReg> for u64 {
    fn from(reg: GpReg) -> u64 {
        reg.0
    }
}

impl fmt::LowerHex for GpReg {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        fmt::LowerHex::fmt(&self.0, f)
    }
}

impl fmt::Debug for GpReg {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{:#018x}", self.0)
    }
}
