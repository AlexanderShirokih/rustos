//! Модель и кодек программы `USRPROG`: точка входа, стек, сегменты и их права.
//!
//! [`ProgramView`] описывает программу без пути и имени, [`validate_program`] проверяет её
//! пригодность к загрузке. [`decode_program`] разбирает байты без аллокации и заимствует их.
//! Фича `alloc` открывает `encode_program`.
//!
//! [`user_vm_window`] вычисляет окно виртуальных адресов между сегментами и стеком.

#![cfg_attr(not(test), no_std)]

#[cfg(any(test, feature = "alloc"))]
extern crate alloc;

mod model;
mod program;

pub use model::{
    Program, ProgramView, Segment, SegmentPermissions, SegmentView, USERLAND_PAGE_SIZE,
    UserVmWindow, UserlandValidationError, user_vm_window, validate_program,
};
pub use program::{
    DecodedProgram, ProgramDecodeError, USERLAND_PROGRAM_MAGIC, USERLAND_PROGRAM_VERSION,
    decode_program,
};
#[cfg(any(test, feature = "alloc"))]
pub use program::{ProgramEncodeError, encode_program};
