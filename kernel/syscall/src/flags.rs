use memory::{
    MemFlags, MemoryProfile,
    mem_flags::{Owners, UncachedPermission},
};
use syscall::UserMemFlags;

pub(super) const fn to_mem_flags(flags: UserMemFlags) -> MemFlags {
    match flags {
        UserMemFlags::ReadWrite => MemFlags::user_rw(),
        UserMemFlags::ReadOnly => MemFlags::user_ro(),
        UserMemFlags::ReadExecute => MemFlags::user_rx(),
    }
}

pub(super) fn mem_flags_for(profile: MemoryProfile, flags: UserMemFlags) -> MemFlags {
    match profile {
        MemoryProfile::Cached => to_mem_flags(flags),
        MemoryProfile::WriteCombining => MemFlags::WriteCombining(uncached_owners(flags)),
        MemoryProfile::Device => MemFlags::Device(uncached_owners(flags)),
    }
}

fn uncached_owners(flags: UserMemFlags) -> Owners<UncachedPermission> {
    // Память вне кэша не исполняется, и `ReadExecute` сводится к read.
    let user = match flags {
        UserMemFlags::ReadWrite => UncachedPermission::writable(),
        UserMemFlags::ReadOnly | UserMemFlags::ReadExecute => UncachedPermission::readonly(),
    };

    Owners {
        kernel: UncachedPermission::default(),
        user,
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
            MemFlags::Cached(o) => UserRwx {
                read: matches!(o.user.access, AccessMode::Readonly | AccessMode::Writable),
                write: matches!(o.user.access, AccessMode::Writable),
                exec: matches!(o.user.executable, Executable::Allowed),
            },
            MemFlags::WriteCombining(_) | MemFlags::Device(_) => UserRwx {
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

    fn uncached_user_access(flags: MemFlags) -> memory::mem_flags::AccessMode {
        match flags {
            MemFlags::WriteCombining(o) | MemFlags::Device(o) => o.user.access,
            MemFlags::Cached(_) => panic!("uncached profile must map to uncached flags"),
        }
    }

    #[test]
    fn cached_region_keeps_cached_flags() {
        assert!(matches!(
            mem_flags_for(MemoryProfile::Cached, UserMemFlags::ReadWrite),
            MemFlags::Cached(_)
        ));
    }

    #[test]
    fn uncached_region_keeps_its_profile() {
        assert!(matches!(
            mem_flags_for(MemoryProfile::Device, UserMemFlags::ReadWrite),
            MemFlags::Device(_)
        ));
        assert!(matches!(
            mem_flags_for(MemoryProfile::WriteCombining, UserMemFlags::ReadWrite),
            MemFlags::WriteCombining(_)
        ));
    }

    #[test]
    fn uncached_region_maps_to_uncached_flags() {
        use memory::mem_flags::AccessMode;
        for profile in [MemoryProfile::Device, MemoryProfile::WriteCombining] {
            assert!(matches!(
                uncached_user_access(mem_flags_for(profile, UserMemFlags::ReadWrite)),
                AccessMode::Writable
            ));
            assert!(matches!(
                uncached_user_access(mem_flags_for(profile, UserMemFlags::ReadOnly)),
                AccessMode::Readonly
            ));
            // Execute не представим вне кэша: сводится к read.
            assert!(matches!(
                uncached_user_access(mem_flags_for(profile, UserMemFlags::ReadExecute)),
                AccessMode::Readonly
            ));
        }
    }
}
