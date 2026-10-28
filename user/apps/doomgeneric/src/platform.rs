//! Платформенные функции `DG_*` движка doomgeneric поверх экрана, клавиш и монотонных часов
//! процесса.
//!
//! Кадр движка выводится на экран модуля `screen`, `DG_GetKey` отдаёт очередь модуля `keys`.
//! Время движка - миллисекунды монотонных часов процесса, сон идёт по тем же часам.
//!
//! Имена функций и буфера кадра задаёт заголовок `doomgeneric.h` движка.

#![allow(unsafe_code, non_snake_case, non_upper_case_globals)]

use core::{
    ffi::{c_char, c_int, c_uchar, c_void},
    ptr, slice,
    time::Duration,
};

use doomgeneric::Mode;
use runtime::Instant;

use crate::{
    keys,
    screen::{self, FRAME_PIXELS},
};

const NS_PER_MS: u64 = 1_000_000;
const MOVE_SPEED_PERCENT: c_int = 70;
const TURN_SPEED_PERCENT: c_int = 50;

unsafe extern "C" {
    static mut DG_ScreenBuffer: *const u32;
    static mut forwardmove: [c_int; 2];
    static mut sidemove: [c_int; 2];
    static mut angleturn: [c_int; 3];
    static mut menuactive: c_int;
    static mut messageToPrint: c_int;
    static mut messageNeedsInput: c_int;
    static mut saveStringEnter: c_int;
    static mut currentMenu: *const c_void;
    static mut MainDef: c_void;
    static mut mousex: c_int;
    static mut mouseSensitivity: c_int;
}

#[allow(clippy::multiple_unsafe_ops_per_block)]
#[unsafe(no_mangle)]
extern "C" fn DG_Init() {
    // SAFETY: g_game.c определяет массивы fixed_t (int) указанных размеров.
    // doomgeneric_Create вызывает DG_Init один раз до D_DoomMain и первого тика.
    // Только поток движка обращается к таблицам; поток Input их не использует.
    unsafe {
        let forward = ptr::addr_of!(forwardmove).read();
        let side = ptr::addr_of!(sidemove).read();
        let turn = ptr::addr_of!(angleturn).read();
        ptr::addr_of_mut!(forwardmove).write(forward.map(|v| v * MOVE_SPEED_PERCENT / 100));
        ptr::addr_of_mut!(sidemove).write(side.map(|v| v * MOVE_SPEED_PERCENT / 100));
        ptr::addr_of_mut!(angleturn).write(turn.map(|v| v * TURN_SPEED_PERCENT / 100));
    }
}

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
    if let Some(controls) = keys::controls(mode()) {
        screen::draw(frame, &controls);
    }
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
    let now = Instant::now().as_nanos() / NS_PER_MS;
    let mode = mode();
    let Some(event) = keys::next(mode, now) else {
        post_turn(mode);
        return 0;
    };
    // SAFETY: `I_GetEvent` движка передаёт указатель на свою локальную переменную `pressed`.
    unsafe { pressed.write(c_int::from(event.pressed)) };
    // SAFETY: `I_GetEvent` движка передаёт указатель на свою локальную переменную `key`.
    unsafe { key.write(event.key) };
    1
}

fn post_turn(mode: Mode) {
    // Поворот касаниями копится в `mousex` движка с его чувствительностью мыши.
    let turn = keys::turn();
    if mode != Mode::Game || turn == 0 {
        return;
    }
    // SAFETY: m_menu.c определяет int `mouseSensitivity`, меню меняет его в потоке движка.
    let sensitivity = unsafe { mouseSensitivity };
    let shift = turn.saturating_mul(sensitivity.saturating_add(5)) / 10;
    // SAFETY: g_game.c определяет int `mousex`, сборка тика читает его в потоке движка.
    unsafe { mousex += shift };
}

#[unsafe(no_mangle)]
extern "C" fn DG_SetWindowTitle(_title: *const c_char) {}

#[allow(clippy::multiple_unsafe_ops_per_block)]
fn mode() -> Mode {
    // SAFETY: флаги m_menu.c имеют C-типы int/boolean (enum с размером int).
    // DG_GetKey и DG_DrawFrame вызываются из единственного потока движка, который
    // изменяет меню. Поток чтения Input не обращается к переменным C.
    // currentMenu хранит указатель menu_t. MainDef используется только как адрес
    // непрозрачного C-объекта menu_t.
    let (menu, message, needs_input, text, root) = unsafe {
        (
            menuactive,
            messageToPrint,
            messageNeedsInput,
            saveStringEnter,
            currentMenu == ptr::addr_of!(MainDef),
        )
    };
    if message != 0 && needs_input != 0 {
        Mode::Confirm
    } else if text != 0 {
        Mode::Text
    } else if menu != 0 && root {
        Mode::MainMenu
    } else if menu != 0 {
        Mode::Menu
    } else {
        Mode::Game
    }
}
