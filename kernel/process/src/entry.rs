//! Параметры первого входа в user-thread - вход для `ArchContext::init_user`.

use core::ptr::NonNull;

use memory::virtual_address::VirtualAddress;

use crate::image::UserImage;

/// Первый аргумент user-thread'а по ABI; `ArchContext::init_user` кладёт его
/// в нужный регистр.
#[derive(Copy, Clone, Debug, PartialEq, Eq)]
#[repr(transparent)]
pub struct UserBootstrapArg(pub u64);

impl UserBootstrapArg {
    pub const ZERO: Self = Self(0);
}

/// Параметры первого входа в user-thread.
pub struct UserEntry {
    /// Вершина kernel-стека потока: на ней работает syscall-обработчик.
    pub kernel_stack_top: NonNull<u8>,
    /// User entry point - куда передаётся управление при первом входе в user-режим.
    pub user_pc: VirtualAddress,
    /// Вершина user-стека, выровненная по требованиям ABI архитектуры.
    pub user_sp: VirtualAddress,
    /// Первый аргумент user-кода по ABI ([`UserBootstrapArg`]).
    pub arg: UserBootstrapArg,
}

/// Собирает [`UserEntry`] из [`UserImage`] и kernel-стека потока.
pub fn user_entry_from_image(
    image: &UserImage<'_>,
    kernel_stack_top: NonNull<u8>,
    arg: UserBootstrapArg,
) -> UserEntry {
    UserEntry {
        kernel_stack_top,
        user_pc: image.entry,
        user_sp: image.user_stack_top,
        arg,
    }
}

#[cfg(test)]
mod tests {
    use memory::{
        MemFlags,
        virtual_address::{PageAlignedVirtualAddress, VirtualAddress},
    };

    use super::*;
    use crate::image::{UserImage, UserSegment};

    const PAGE: usize = 4096;

    fn aligned(addr: usize) -> PageAlignedVirtualAddress {
        PageAlignedVirtualAddress::from_usize(addr).expect("aligned addr")
    }

    fn fake_kernel_top() -> NonNull<u8> {
        NonNull::new(0x8000_0000usize as *mut u8).expect("non-null pointer literal")
    }

    fn make_image<'a>(segs: &'a [UserSegment<'a>]) -> UserImage<'a> {
        UserImage {
            segments: segs,
            entry: VirtualAddress::new(0x4000_0000),
            user_stack_top: VirtualAddress::new(0x1_0000_0000),
            user_stack_size: 4 * PAGE,
        }
    }

    #[test]
    fn from_image_propagates_pc_sp_and_arg() {
        let segs = [UserSegment {
            va_base: aligned(0x4000_0000),
            mapped_size: PAGE,
            init_bytes: &[],
            perms: MemFlags::user_rx(),
        }];
        let image = make_image(&segs);

        let arg = UserBootstrapArg(0xDEAD_BEEF);
        let entry = user_entry_from_image(&image, fake_kernel_top(), arg);

        assert_eq!(entry.user_pc.as_usize(), 0x4000_0000);
        assert_eq!(entry.user_sp.as_usize(), 0x1_0000_0000);
        assert_eq!(entry.arg, arg);
        assert_eq!(entry.kernel_stack_top.as_ptr() as usize, 0x8000_0000);
    }
}
