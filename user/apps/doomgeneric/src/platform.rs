//! Платформенные функции `DG_*` движка doomgeneric поверх экрана, клавиш и монотонных часов
//! процесса.
//!
//! Кадр движка выводится на экран модуля `screen`, `DG_GetKey` отдаёт очередь модуля `keys`.
//! Время движка - миллисекунды монотонных часов процесса, сон идёт по тем же часам.
//!
//! Имена функций и буфера кадра задаёт заголовок `doomgeneric.h` движка.

#![allow(unsafe_code, non_snake_case, non_upper_case_globals)]

use core::{
    ffi::{c_char, c_int, c_uchar},
    slice,
    time::Duration,
};

use runtime::Instant;

use crate::{
    keys,
    screen::{self, FRAME_PIXELS},
};

const NS_PER_MS: u64 = 1_000_000;

unsafe extern "C" {
    static mut DG_ScreenBuffer: *const u32;
}

#[unsafe(no_mangle)]
extern "C" fn DG_Init() {}

#[unsafe(no_mangle)]
extern "C" fn DG_DrawFrame() {
    // SAFETY: движок пишет указатель один раз в `doomgeneric_Create` до первого кадра и читает
    // его из того же потока, что вызывает `DG_DrawFrame`.
    let buffer = unsafe { DG_ScreenBuffer };
    if buffer.is_null() {
        return;
    }

    // SAFETY: `doomgeneric_Create` выделяет буфер `DOOMGENERIC_RESX * DOOMGENERIC_RESY`
    // пикселей `uint32_t`. `build.rs` задаёт эти макросы константами `frame`, из которых
    // сложен `FRAME_PIXELS`. Движок не освобождает буфер до выхода и не меняет его во время
    // вызова `DG_DrawFrame` из своего потока.
    let frame = unsafe { slice::from_raw_parts(buffer, FRAME_PIXELS) };
    screen::draw(frame);
}

#[unsafe(no_mangle)]
extern "C" fn DG_SleepMs(ms: u32) {
    runtime::sleep(Duration::from_millis(ms.into()));
}

#[unsafe(no_mangle)]
extern "C" fn DG_GetTicksMs() -> u32 {
    // Движок считает время разностью показаний, и показание - младшие 32 бита миллисекунд.
    let [b0, b1, b2, b3, ..] = (Instant::now().as_nanos() / NS_PER_MS).to_le_bytes();
    u32::from_le_bytes([b0, b1, b2, b3])
}

#[unsafe(no_mangle)]
extern "C" fn DG_GetKey(pressed: *mut c_int, key: *mut c_uchar) -> c_int {
    let Some(event) = keys::next() else {
        return 0;
    };
    // SAFETY: `I_GetEvent` движка передаёт указатель на свою локальную переменную `pressed`.
    unsafe { pressed.write(c_int::from(event.pressed)) };
    // SAFETY: `I_GetEvent` движка передаёт указатель на свою локальную переменную `key`.
    unsafe { key.write(event.key) };
    1
}

#[unsafe(no_mangle)]
extern "C" fn DG_SetWindowTitle(_title: *const c_char) {}
