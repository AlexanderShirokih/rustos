//! Сырой доступ к per-thread IPC-буферу: тело, caps.
//!
//! Общий модуль testrunner'а и его фикстур: обе стороны кладут и читают кадры
//! без типизированного контракта, потому что протокол фикстуры - один байт
//! опкода и опциональный `u64`, заводить под него `#[contract]` избыточно.

// Модуль подключён к обоим бинарям, и каждый пользуется своим подмножеством.
#![allow(dead_code)]

use runtime::ipc_buffer_ptr;
use syscall::{IpcBuffer, decode_tag, encode_tag};

/// Указатель на буфер текущего потока; отсутствие буфера - контрактная ошибка
/// вызывающего, а не восстановимое состояние.
fn buffer() -> *mut IpcBuffer {
    ipc_buffer_ptr().expect("thread has an IPC buffer")
}

/// Кладёт `body` и `caps` в буфер текущего потока.
pub fn write_frame(body: &[u8], caps: &[u32]) {
    let ptr = buffer();
    // SAFETY: ядро замаппило буфер user-RW на страницу; поток - единственный
    // его владелец, запись идёт в границах полей.
    unsafe {
        let buf = &mut *ptr;
        buf.data[..body.len()].copy_from_slice(body);
        buf.caps[..caps.len()].copy_from_slice(caps);
        buf.tag = encode_tag(body.len(), caps.len());
    }
}

/// Читает из буфера текущего потока `min(len, out.len())` байт тела.
/// Возвращает полную длину тела и число доставленных caps.
pub fn read_frame(out: &mut [u8]) -> (usize, usize) {
    let ptr = buffer();
    // SAFETY: см. [`write_frame`]; здесь только чтение полей.
    unsafe {
        let buf = &*ptr;
        let decoded = decode_tag(buf.tag);
        let copy = decoded.len.min(out.len());
        out[..copy].copy_from_slice(&buf.data[..copy]);
        (decoded.len, decoded.ncaps)
    }
}

/// Читает `caps[index]` из буфера текущего потока.
pub fn read_cap(index: usize) -> u32 {
    let ptr = buffer();
    // SAFETY: см. [`write_frame`]; здесь только чтение полей.
    unsafe { (*ptr).caps[index] }
}
