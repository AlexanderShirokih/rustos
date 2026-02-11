#[derive(Copy, Clone, Debug)]
pub enum AccessMode {
    None,
    Readonly,
    Writable,
}

#[derive(Copy, Clone, Debug)]
pub enum Executable {
    Allowed,
    NotAllowed,
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

#[derive(Copy, Clone, Debug)]
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

impl Default for DeviceMemoryPermission {
    fn default() -> Self {
        Self {
            access: AccessMode::None,
        }
    }
}

#[derive(Copy, Clone, Debug)]
pub struct Owners<T: Default> {
    pub kernel: T,
    pub user: T,
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
