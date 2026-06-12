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

#[derive(Copy, Clone)]
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
pub struct Owners<T: Default + Copy> {
    pub kernel: T,
    pub user: T,
}

impl<T: Default + Copy + Display> Display for Owners<T> {
    fn fmt(&self, f: &mut Formatter<'_>) -> core::fmt::Result {
        write!(f, "[kernel: {}; user: {}]", self.kernel, self.user)
    }
}

impl<T: Default + Copy> Owners<T> {
    pub fn kernel(kernel: T) -> Self {
        Self {
            kernel,
            user: Default::default(),
        }
    }
}

#[derive(Copy, Clone)]
pub enum MemFlags {
    Private(Owners<PrivateMemoryPermission>),
    Device(Owners<DeviceMemoryPermission>),
}

impl MemFlags {
    /// User read-only, без исполнения. Kernel-режим не имеет прав через user-mapping -
    /// для kernel-side доступа используйте линейную карту higher-half.
    pub const fn user_ro() -> Self {
        MemFlags::Private(Owners {
            kernel: PrivateMemoryPermission {
                access: AccessMode::None,
                executable: Executable::NotAllowed,
            },
            user: PrivateMemoryPermission {
                access: AccessMode::Readonly,
                executable: Executable::NotAllowed,
            },
        })
    }

    /// User read/write, без исполнения.
    pub const fn user_rw() -> Self {
        MemFlags::Private(Owners {
            kernel: PrivateMemoryPermission {
                access: AccessMode::None,
                executable: Executable::NotAllowed,
            },
            user: PrivateMemoryPermission {
                access: AccessMode::Writable,
                executable: Executable::NotAllowed,
            },
        })
    }

    /// User read + execute (для исполняемых сегментов user-кода).
    pub const fn user_rx() -> Self {
        MemFlags::Private(Owners {
            kernel: PrivateMemoryPermission {
                access: AccessMode::None,
                executable: Executable::NotAllowed,
            },
            user: PrivateMemoryPermission {
                access: AccessMode::Readonly,
                executable: Executable::Allowed,
            },
        })
    }

    /// Kernel read/write, без исполнения, без user-доступа. Использует
    /// kernel-heap для собственной арены и любые kernel-only данные.
    pub const fn kernel_rw() -> Self {
        MemFlags::Private(Owners {
            kernel: PrivateMemoryPermission {
                access: AccessMode::Writable,
                executable: Executable::NotAllowed,
            },
            user: PrivateMemoryPermission {
                access: AccessMode::None,
                executable: Executable::NotAllowed,
            },
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn private_perms(flags: &MemFlags) -> &Owners<PrivateMemoryPermission> {
        match flags {
            MemFlags::Private(p) => p,
            MemFlags::Device(_) => panic!("expected Private flags"),
        }
    }

    #[test]
    fn user_ro_grants_only_user_read() {
        let flags = MemFlags::user_ro();
        let p = private_perms(&flags);
        assert!(matches!(p.kernel.access, AccessMode::None));
        assert!(matches!(p.kernel.executable, Executable::NotAllowed));
        assert!(matches!(p.user.access, AccessMode::Readonly));
        assert!(matches!(p.user.executable, Executable::NotAllowed));
    }

    #[test]
    fn user_rw_grants_user_write_no_exec() {
        let flags = MemFlags::user_rw();
        let p = private_perms(&flags);
        assert!(matches!(p.kernel.access, AccessMode::None));
        assert!(matches!(p.user.access, AccessMode::Writable));
        assert!(matches!(p.user.executable, Executable::NotAllowed));
    }

    #[test]
    fn user_rx_grants_user_read_and_exec() {
        let flags = MemFlags::user_rx();
        let p = private_perms(&flags);
        assert!(matches!(p.kernel.access, AccessMode::None));
        assert!(matches!(p.user.access, AccessMode::Readonly));
        assert!(matches!(p.user.executable, Executable::Allowed));
    }

    #[test]
    fn user_helpers_grant_no_kernel_permissions() {
        for flags in [
            MemFlags::user_ro(),
            MemFlags::user_rw(),
            MemFlags::user_rx(),
        ] {
            let p = private_perms(&flags);
            assert!(matches!(p.kernel.access, AccessMode::None));
            assert!(matches!(p.kernel.executable, Executable::NotAllowed));
        }
    }

    #[test]
    fn kernel_rw_grants_only_kernel_write_no_exec() {
        let flags = MemFlags::kernel_rw();
        let p = private_perms(&flags);
        assert!(matches!(p.kernel.access, AccessMode::Writable));
        assert!(matches!(p.kernel.executable, Executable::NotAllowed));
        assert!(matches!(p.user.access, AccessMode::None));
        assert!(matches!(p.user.executable, Executable::NotAllowed));
    }
}
