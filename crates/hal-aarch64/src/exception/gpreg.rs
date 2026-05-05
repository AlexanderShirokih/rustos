use core::fmt;

/// Значение регистра общего назначения (x0-x30).
#[repr(transparent)]
#[derive(Clone, Copy, PartialEq, Eq)]
pub struct GpReg(u64);

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
