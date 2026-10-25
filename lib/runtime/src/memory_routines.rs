//! Копирование и заполнение памяти для кода компилятора и библиотеки C: `memcpy`, `memmove` и
//! `memset`. Процедуры обращаются к памяти невыровненными парными и векторными обращениями и
//! годятся только для обычной памяти. Определения перекрывают слабые символы `compiler_builtins`.
//!
//! Тела процедур в `memcpy.s` и `memset.s` - Arm Optimized Routines в редакции picolibc 1.8.12
//! под лицензией MIT.

use core::{
    arch::naked_asm,
    ffi::{c_int, c_void},
};

#[unsafe(no_mangle)]
#[unsafe(naked)]
unsafe extern "C" fn memcpy(
    _destination: *mut c_void,
    _source: *const c_void,
    _count: usize,
) -> *mut c_void {
    naked_asm!(include_str!("memcpy.s"))
}

#[unsafe(no_mangle)]
#[unsafe(naked)]
unsafe extern "C" fn memmove(
    _destination: *mut c_void,
    _source: *const c_void,
    _count: usize,
) -> *mut c_void {
    // Копия до 128 байт загружает весь диапазон в регистры до первой записи, длинная копия при
    // приёмнике выше источника с перекрытием идёт с конца, и вход `memcpy` обслуживает
    // перекрывающиеся диапазоны.
    naked_asm!("b {copy}", copy = sym memcpy)
}

#[unsafe(no_mangle)]
#[unsafe(naked)]
unsafe extern "C" fn memset(
    _destination: *mut c_void,
    _value: c_int,
    _count: usize,
) -> *mut c_void {
    naked_asm!(include_str!("memset.s"))
}
