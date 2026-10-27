//! Модель программы и разбор её ELF: точка входа, стек, сегменты и их права.
//!
//! [`ProgramView`] описывает программу без пути и имени, [`validate_program`] проверяет её
//! пригодность к загрузке. [`decode_program`] разбирает байты ELF без аллокации и заимствует их.
//! Фича `alloc` открывает `encode_elf_program`.
//!
//! [`user_vm_window`] вычисляет окно виртуальных адресов между сегментами и стеком.

#![cfg_attr(not(test), no_std)]

#[cfg(any(test, feature = "alloc"))]
extern crate alloc;

mod elf;
#[cfg(any(test, feature = "alloc"))]
mod elf_write;
mod model;
mod program;
#[cfg(any(test, feature = "alloc"))]
mod program_encode;

pub use model::{
    Program, ProgramView, Segment, SegmentPermissions, SegmentView, USERLAND_PAGE_SIZE,
    UserVmWindow, UserlandValidationError, user_vm_window, validate_program,
};
pub use program::{DecodedProgram, ProgramDecodeError, decode_program};
#[cfg(any(test, feature = "alloc"))]
pub use program_encode::{ProgramEncodeError, encode_elf_program};
