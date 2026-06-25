use memory::MemFlags;
use syscall::UserMemFlags;

/// Преобразует ABI-флаги user-памяти в [`MemFlags`] страничной таблицы.
pub(super) const fn to_mem_flags(flags: UserMemFlags) -> MemFlags {
    match flags {
        UserMemFlags::ReadWrite => MemFlags::user_rw(),
        UserMemFlags::ReadOnly => MemFlags::user_ro(),
        UserMemFlags::ReadExecute => MemFlags::user_rx(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn user_rwx(flags: MemFlags) -> (bool, bool, bool) {
        use memory::mem_flags::{AccessMode, Executable};
        match flags {
            MemFlags::Private(o) => (
                matches!(o.user.access, AccessMode::Readonly | AccessMode::Writable),
                matches!(o.user.access, AccessMode::Writable),
                matches!(o.user.executable, Executable::Allowed),
            ),
            MemFlags::Device(_) => (false, false, false),
        }
    }

    #[test]
    fn map_to_matching_mem_flags() {
        assert_eq!(
            user_rwx(to_mem_flags(UserMemFlags::ReadWrite)),
            (true, true, false)
        );
        assert_eq!(
            user_rwx(to_mem_flags(UserMemFlags::ReadOnly)),
            (true, false, false)
        );
        assert_eq!(
            user_rwx(to_mem_flags(UserMemFlags::ReadExecute)),
            (true, false, true)
        );
    }
}
