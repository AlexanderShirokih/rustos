use memory::{
    MemFlags, MemoryType,
    mem_flags::{DeviceMemoryPermission, Owners},
};
use syscall::UserMemFlags;

/// Преобразует ABI-флаги user-памяти в Normal [`MemFlags`] страничной таблицы.
pub(super) const fn to_mem_flags(flags: UserMemFlags) -> MemFlags {
    match flags {
        UserMemFlags::ReadWrite => MemFlags::user_rw(),
        UserMemFlags::ReadOnly => MemFlags::user_ro(),
        UserMemFlags::ReadExecute => MemFlags::user_rx(),
    }
}

/// Выбирает [`MemFlags`] по типу памяти региона: Device получает
/// Device-nGnRE-атрибут, Normal - cacheable. Device не исполняется, поэтому
/// `ReadExecute` сводится к read.
pub(super) fn mem_flags_for(memory_type: MemoryType, flags: UserMemFlags) -> MemFlags {
    match memory_type {
        MemoryType::Normal => to_mem_flags(flags),
        MemoryType::Device => {
            let user = match flags {
                UserMemFlags::ReadWrite => DeviceMemoryPermission::writable(),
                UserMemFlags::ReadOnly | UserMemFlags::ReadExecute => {
                    DeviceMemoryPermission::readonly()
                }
            };

            MemFlags::Device(Owners {
                kernel: DeviceMemoryPermission::default(),
                user,
            })
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[derive(Debug, PartialEq, Eq)]
    struct UserRwx {
        read: bool,
        write: bool,
        exec: bool,
    }

    fn user_rwx(flags: MemFlags) -> UserRwx {
        use memory::mem_flags::{AccessMode, Executable};
        match flags {
            MemFlags::Private(o) => UserRwx {
                read: matches!(o.user.access, AccessMode::Readonly | AccessMode::Writable),
                write: matches!(o.user.access, AccessMode::Writable),
                exec: matches!(o.user.executable, Executable::Allowed),
            },
            MemFlags::Device(_) => UserRwx {
                read: false,
                write: false,
                exec: false,
            },
        }
    }

    #[test]
    fn map_to_matching_mem_flags() {
        assert_eq!(
            user_rwx(to_mem_flags(UserMemFlags::ReadWrite)),
            UserRwx {
                read: true,
                write: true,
                exec: false,
            }
        );
        assert_eq!(
            user_rwx(to_mem_flags(UserMemFlags::ReadOnly)),
            UserRwx {
                read: true,
                write: false,
                exec: false,
            }
        );
        assert_eq!(
            user_rwx(to_mem_flags(UserMemFlags::ReadExecute)),
            UserRwx {
                read: true,
                write: false,
                exec: true,
            }
        );
    }

    fn device_user_access(flags: MemFlags) -> memory::mem_flags::AccessMode {
        match flags {
            MemFlags::Device(o) => o.user.access,
            MemFlags::Private(_) => panic!("device region must map to Device flags"),
        }
    }

    #[test]
    fn normal_region_keeps_private_flags() {
        assert!(matches!(
            mem_flags_for(MemoryType::Normal, UserMemFlags::ReadWrite),
            MemFlags::Private(_)
        ));
    }

    #[test]
    fn device_region_maps_to_device_flags() {
        use memory::mem_flags::AccessMode;
        assert!(matches!(
            device_user_access(mem_flags_for(MemoryType::Device, UserMemFlags::ReadWrite)),
            AccessMode::Writable
        ));
        assert!(matches!(
            device_user_access(mem_flags_for(MemoryType::Device, UserMemFlags::ReadOnly)),
            AccessMode::Readonly
        ));
        // Execute не представим для device-памяти: сводится к read.
        assert!(matches!(
            device_user_access(mem_flags_for(MemoryType::Device, UserMemFlags::ReadExecute)),
            AccessMode::Readonly
        ));
    }
}
