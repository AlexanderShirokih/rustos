use core::fmt::{Display, Formatter};

#[derive(Copy, Clone)]
pub enum AccessMode {
    None,
    Readonly,
    Writable,
}

impl Display for AccessMode {
    fn fmt(&self, f: &mut Formatter<'_>) -> core::fmt::Result {
        match self {
            AccessMode::None => f.write_str("None"),
            AccessMode::Readonly => f.write_str("RO"),
            AccessMode::Writable => f.write_str("RW"),
        }
    }
}

#[derive(Copy, Clone)]
pub enum Executable {
    Allowed,
    NotAllowed,
}

impl Display for Executable {
    fn fmt(&self, f: &mut Formatter<'_>) -> core::fmt::Result {
        match self {
            Executable::Allowed => f.write_str("E"),
            Executable::NotAllowed => f.write_str("nE"),
        }
    }
}

pub struct PrivateMemoryPermission {
    pub access: AccessMode,
    pub executable: Executable,
}

impl Default for PrivateMemoryPermission {
    fn default() -> Self {
        Self {
            access: AccessMode::None,
            executable: Executable::NotAllowed,
        }
    }
}

#[derive(Copy, Clone)]
pub struct DeviceMemoryPermission {
    pub access: AccessMode,
}

impl DeviceMemoryPermission {
    pub const fn readonly() -> Self {
        Self {
            access: AccessMode::Readonly,
        }
    }

    pub const fn writable() -> Self {
        Self {
            access: AccessMode::Writable,
        }
    }
}

impl Display for DeviceMemoryPermission {
    fn fmt(&self, f: &mut Formatter<'_>) -> core::fmt::Result {
        write!(f, "[DMA: {}]", self.access)
    }
}

impl Default for DeviceMemoryPermission {
    fn default() -> Self {
        Self {
            access: AccessMode::None,
        }
    }
}

#[derive(Copy, Clone)]
pub struct Owners<T: Default> {
    pub kernel: T,
    pub user: T,
}

impl<T: Default + Display> Display for Owners<T> {
    fn fmt(&self, f: &mut Formatter<'_>) -> core::fmt::Result {
        write!(f, "[kernel: {}; user: {}]", self.kernel, self.user)
    }
}

impl<T: Default> Owners<T> {
    pub fn kernel(kernel: T) -> Self {
        Self {
            kernel,
            user: Default::default(),
        }
    }
}

pub enum MemFlags {
    Private(Owners<PrivateMemoryPermission>),
    Device(Owners<DeviceMemoryPermission>),
}
