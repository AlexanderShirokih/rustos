//! Класс файлов WAD `stdc_wad_file` и проверка `DG_IwadExists` отдают движку IWAD, встроенный
//! в образ программы. `build.rs` подставляет `DG_IwadExists` вместо `M_FileExists` в поиск
//! IWAD.
//!
//! Имя `IWAD` открывает встроенный IWAD, других файлов WAD нет. Раскладку
//! класса и файла задаёт заголовок `w_file.h` движка.

#![allow(unsafe_code, non_snake_case, non_upper_case_globals)]

use alloc::boxed::Box;
use core::{
    ffi::{CStr, c_char, c_int, c_uint, c_void},
    ptr,
};

pub(crate) const IWAD: &CStr = c"freedoom1.wad";

static IWAD_DATA: &[u8] = include_bytes!(concat!(env!("OUT_DIR"), "/freedoom1.wad"));

#[repr(C)]
pub(crate) struct WadFileClass {
    open: unsafe extern "C" fn(*const c_char) -> *mut WadFile,
    close: unsafe extern "C" fn(*mut WadFile),
    read: unsafe extern "C" fn(*mut WadFile, c_uint, *mut c_void, usize) -> usize,
}

#[repr(C)]
pub(crate) struct WadFile {
    class: *const WadFileClass,
    mapped: *mut u8,
    length: c_uint,
}

#[unsafe(no_mangle)]
static stdc_wad_file: WadFileClass = WadFileClass { open, close, read };

#[unsafe(no_mangle)]
unsafe extern "C" fn DG_IwadExists(name: *const c_char) -> c_int {
    // SAFETY: `d_iwad.c` передаёт имя IWAD строкой C, завершённой нулём.
    c_int::from(!name.is_null() && unsafe { CStr::from_ptr(name) } == IWAD)
}

unsafe extern "C" fn open(path: *const c_char) -> *mut WadFile {
    // SAFETY: `W_OpenFile` передаёт путь строкой C, завершённой нулём.
    let Some(length) = (!path.is_null() && unsafe { CStr::from_ptr(path) } == IWAD)
        .then(|| c_uint::try_from(IWAD_DATA.len()).ok())
        .flatten()
    else {
        return ptr::null_mut();
    };
    // Без `mapped` движок копирует лампы в свою выровненную память.
    Box::into_raw(Box::new(WadFile {
        class: &raw const stdc_wad_file,
        mapped: ptr::null_mut(),
        length,
    }))
}

unsafe extern "C" fn close(file: *mut WadFile) {
    // SAFETY: `W_CloseFile` передаёт файл, который вернул `open`, один раз.
    drop(unsafe { Box::from_raw(file) });
}

unsafe extern "C" fn read(
    _file: *mut WadFile,
    offset: c_uint,
    buffer: *mut c_void,
    length: usize,
) -> usize {
    let data = usize::try_from(offset)
        .ok()
        .and_then(|offset| IWAD_DATA.get(offset..))
        .unwrap_or_default();
    let count = length.min(data.len());
    // SAFETY: `W_Read` передаёт буфер не короче `length` байт, `count` не больше `length` и
    // длины остатка IWAD.
    unsafe { ptr::copy_nonoverlapping(data.as_ptr(), buffer.cast::<u8>(), count) };
    count
}
