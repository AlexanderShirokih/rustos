#![no_std]
#![no_main]
#![allow(unsafe_code)]

extern crate alloc;

use core::{fmt::Write as _, panic::PanicInfo};

use bootstrap::{BootstrapClient, LOG_MESSAGE_MAX};
use io::writer::Writer;
use ipc::wire::{IpcError, Str};
use kernel_tests::kernel_test;
use runtime::{PortTransport, thread_exit};
use spin::{Mutex, Once};
use syscall::Handle;

mod heap;
mod ipc_buffer;
mod irq_provision;
mod load_entry;
mod memory_capability_target;
mod owned_handle;
mod pl031_rtc;
mod port;
mod process_handles;
mod process_start;
mod ring_transport;
mod signal;
mod spawn_from_image;
mod sync;

/// HandleId WRITE-конца bootstrap-канала, полученный в `_start`; ставится один раз.
static BOOTSTRAP_HANDLE: Once<Handle> = Once::new();

/// Накопитель строки лога: байты копятся до `\n` либо заполнения, затем
/// уходят одним cast `log` контракта `Bootstrap`.
struct LogBuffer {
    len: usize,
    bytes: [u8; LOG_MESSAGE_MAX],
}

static LOG_BUFFER: Mutex<LogBuffer> = Mutex::new(LogBuffer {
    len: 0,
    bytes: [0; LOG_MESSAGE_MAX],
});

/// Writer harness'а: шлёт построчные cast'ы `log` в bootstrap-канал.
struct ChannelLogWriter;

static LOG_WRITER: ChannelLogWriter = ChannelLogWriter;

impl Writer for ChannelLogWriter {
    fn write_all(&self, buf: &[u8]) {
        let mut state = LOG_BUFFER.lock();
        for &byte in buf {
            if byte == b'\n' {
                flush_line(&mut state);
                continue;
            }
            let len = state.len;
            state.bytes[len] = byte;
            state.len += 1;
            if state.len == LOG_MESSAGE_MAX {
                flush_line(&mut state);
            }
        }
    }

    fn flush(&self) {
        flush_line(&mut LOG_BUFFER.lock());
    }
}

/// Отправляет накопленную строку cast'ом `log` и очищает буфер.
fn flush_line(state: &mut LogBuffer) {
    if state.len == 0 {
        return;
    }
    send_log_frame(&state.bytes[..state.len]);
    state.len = 0;
}

/// Best-effort отправка строки лога контрактом `Bootstrap`: на WouldBlock -
/// ограниченный спин-retry, иная ошибка либо не-UTF8 молча дропает кадр.
fn send_log_frame(payload: &[u8]) {
    const SEND_RETRY_LIMIT: usize = 1024;

    let Ok(text) = core::str::from_utf8(payload) else {
        return;
    };
    let Some(message) = Str::<LOG_MESSAGE_MAX>::new(text) else {
        return;
    };

    let Some(&handle) = BOOTSTRAP_HANDLE.get() else {
        return;
    };
    let client = BootstrapClient::new(PortTransport::client(handle));

    for _ in 0..SEND_RETRY_LIMIT {
        match client.log(message) {
            Err(IpcError::WouldBlock) => core::hint::spin_loop(),
            _ => return,
        }
    }
}

/// Handle WRITE-конца bootstrap-канала для тестов, обслуживающих контракт `Bootstrap`.
pub(crate) fn bootstrap_handle() -> Handle {
    *BOOTSTRAP_HANDLE
        .get()
        .expect("bootstrap handle set in _start")
}

fn runner_exit(code: u32) -> ! {
    LOG_WRITER.flush();
    thread_exit(u64::from(code))
}

/// `bootstrap` приходит в x0 как HandleId WRITE-конца канала, переданного ядром при спавне.
#[unsafe(no_mangle)]
pub extern "C" fn _start(bootstrap: Handle) -> ! {
    BOOTSTRAP_HANDLE.call_once(|| bootstrap);
    kernel_tests::runner::install_writer(&LOG_WRITER);
    kernel_tests::runner::install_exit(runner_exit);
    kernel_tests::run_all_tests()
}

#[kernel_test]
fn userland_smoke() {
    let sum: u64 = (1..=10).sum();
    kernel_tests::kassert_eq!(sum, 55);
    kernel_tests::kassert!(sum.is_multiple_of(5));
}

/// Стековый форматтер сообщения паники: излишек сверх ёмкости кадра
/// молча обрезается.
struct PanicBuffer {
    len: usize,
    bytes: [u8; LOG_MESSAGE_MAX],
}

impl core::fmt::Write for PanicBuffer {
    fn write_str(&mut self, s: &str) -> core::fmt::Result {
        let take = s.len().min(LOG_MESSAGE_MAX - self.len);
        self.bytes[self.len..self.len + take].copy_from_slice(&s.as_bytes()[..take]);
        self.len += take;
        Ok(())
    }
}

#[panic_handler]
fn panic(info: &PanicInfo<'_>) -> ! {
    let mut buf = PanicBuffer {
        len: 0,
        bytes: [0; LOG_MESSAGE_MAX],
    };
    let _ = write!(buf, "[TEST-PANIC] {info}");
    send_log_frame(&buf.bytes[..buf.len]);
    thread_exit(1)
}
