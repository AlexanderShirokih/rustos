use core::fmt::{Display, Formatter};

#[derive(Copy, Clone)]
pub enum AccessMode {
    None,
    Readonly,
    Writable,
}

#[derive(Copy, Clone)]
pub enum Executable {
    Allowed,
    NotAllowed,
}

/// Права владельца на кэшируемую память.
#[derive(Copy, Clone)]
pub struct CachedPermission {
    pub access: AccessMode,
    pub executable: Executable,
}

/// Права владельца на память вне кэша процессора. Исполнение не представимо.
#[derive(Copy, Clone)]
pub struct UncachedPermission {
    pub access: AccessMode,
}

/// Права ядра и процесса на один и тот же диапазон.
#[derive(Copy, Clone)]
pub struct Owners<T: Default + Copy> {
    pub kernel: T,
    pub user: T,
}

/// Атрибуты страницы: профиль работы с памятью и права владельцев.
#[derive(Copy, Clone)]
pub enum MemFlags {
    /// Кэшируемая память с обратной записью.
    Cached(Owners<CachedPermission>),
    /// Некэшируемая память с объединением записей.
    WriteCombining(Owners<UncachedPermission>),
    /// Регистры устройства.
    Device(Owners<UncachedPermission>),
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

impl Display for Executable {
    fn fmt(&self, f: &mut Formatter<'_>) -> core::fmt::Result {
        match self {
            Executable::Allowed => f.write_str("E"),
            Executable::NotAllowed => f.write_str("nE"),
        }
    }
}

impl Default for CachedPermission {
    fn default() -> Self {
        Self {
            access: AccessMode::None,
            executable: Executable::NotAllowed,
        }
    }
}

impl UncachedPermission {
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

impl Default for UncachedPermission {
    fn default() -> Self {
        Self {
            access: AccessMode::None,
        }
    }
}

impl Display for UncachedPermission {
    fn fmt(&self, f: &mut Formatter<'_>) -> core::fmt::Result {
        write!(f, "{}", self.access)
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

impl<T: Default + Copy + Display> Display for Owners<T> {
    fn fmt(&self, f: &mut Formatter<'_>) -> core::fmt::Result {
        write!(f, "[kernel: {}; user: {}]", self.kernel, self.user)
    }
}

impl MemFlags {
    /// User read-only, без исполнения. Kernel-режим не имеет прав через
    /// user-mapping - для kernel-side доступа используйте линейную карту
    /// higher-half.
    pub const fn user_ro() -> Self {
        MemFlags::Cached(Owners {
            kernel: CachedPermission {
                access: AccessMode::None,
                executable: Executable::NotAllowed,
            },
            user: CachedPermission {
                access: AccessMode::Readonly,
                executable: Executable::NotAllowed,
            },
        })
    }

    /// User read/write, без исполнения.
    pub const fn user_rw() -> Self {
        MemFlags::Cached(Owners {
            kernel: CachedPermission {
                access: AccessMode::None,
                executable: Executable::NotAllowed,
            },
            user: CachedPermission {
                access: AccessMode::Writable,
                executable: Executable::NotAllowed,
            },
        })
    }

    /// User read + execute (для исполняемых сегментов user-кода).
    pub const fn user_rx() -> Self {
        MemFlags::Cached(Owners {
            kernel: CachedPermission {
                access: AccessMode::None,
                executable: Executable::NotAllowed,
            },
            user: CachedPermission {
                access: AccessMode::Readonly,
                executable: Executable::Allowed,
            },
        })
    }

    /// Kernel read/write, без исполнения, без user-доступа. Подходит для kernel-heap:
    /// собственной арены и произвольных kernel-only данных.
    pub const fn kernel_rw() -> Self {
        MemFlags::Cached(Owners {
            kernel: CachedPermission {
                access: AccessMode::Writable,
                executable: Executable::NotAllowed,
            },
            user: CachedPermission {
                access: AccessMode::None,
                executable: Executable::NotAllowed,
            },
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn cached_perms(flags: &MemFlags) -> &Owners<CachedPermission> {
        match flags {
            MemFlags::Cached(p) => p,
            MemFlags::WriteCombining(_) | MemFlags::Device(_) => panic!("expected Cached flags"),
        }
    }

    #[test]
    fn user_ro_grants_only_user_read() {
        let flags = MemFlags::user_ro();
        let p = cached_perms(&flags);
        assert!(matches!(p.kernel.access, AccessMode::None));
        assert!(matches!(p.kernel.executable, Executable::NotAllowed));
        assert!(matches!(p.user.access, AccessMode::Readonly));
        assert!(matches!(p.user.executable, Executable::NotAllowed));
    }

    #[test]
    fn user_rw_grants_user_write_no_exec() {
        let flags = MemFlags::user_rw();
        let p = cached_perms(&flags);
        assert!(matches!(p.kernel.access, AccessMode::None));
        assert!(matches!(p.user.access, AccessMode::Writable));
        assert!(matches!(p.user.executable, Executable::NotAllowed));
    }

    #[test]
    fn user_rx_grants_user_read_and_exec() {
        let flags = MemFlags::user_rx();
        let p = cached_perms(&flags);
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
            let p = cached_perms(&flags);
            assert!(matches!(p.kernel.access, AccessMode::None));
            assert!(matches!(p.kernel.executable, Executable::NotAllowed));
        }
    }

    #[test]
    fn kernel_rw_grants_only_kernel_write_no_exec() {
        let flags = MemFlags::kernel_rw();
        let p = cached_perms(&flags);
        assert!(matches!(p.kernel.access, AccessMode::Writable));
        assert!(matches!(p.kernel.executable, Executable::NotAllowed));
        assert!(matches!(p.user.access, AccessMode::None));
        assert!(matches!(p.user.executable, Executable::NotAllowed));
    }
}
