#![no_std]
#![no_main]
#![allow(unsafe_code)]

use core::{
    fmt::Write as _,
    panic::PanicInfo,
    sync::atomic::{AtomicUsize, Ordering},
};

use bootstrap::{BootstrapClient, LOG_MESSAGE_MAX};
use io::writer::Writer;
use ipc::wire::{IpcError, Str};
use kernel_tests::kernel_test;
use runtime::{ChannelTransport, thread_exit};
use spin::Mutex;

mod channel;
mod mailbox;
mod memory_kobject;
mod process_handles;
mod self_spawn;

/// Сырой HandleId WRITE-конца bootstrap-канала, полученный в `_start`.
static BOOTSTRAP_HANDLE: AtomicUsize = AtomicUsize::new(0);

/// Накопитель строки лога: байты копятся до `\n` либо заполнения, затем
/// уходят одним RKLOG-кадром.
struct LogBuffer {
    len: usize,
    bytes: [u8; LOG_MESSAGE_MAX],
}

static LOG_BUFFER: Mutex<LogBuffer> = Mutex::new(LogBuffer {
    len: 0,
    bytes: [0; LOG_MESSAGE_MAX],
});

/// Writer harness'а: шлёт построчные RKLOG-кадры в bootstrap-канал.
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

/// Отправляет накопленную строку RKLOG-кадром и очищает буфер; пустой
/// буфер кадра не порождает.
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
    let Ok(message) = Str::<LOG_MESSAGE_MAX>::new(text) else {
        return;
    };

    let handle = BOOTSTRAP_HANDLE.load(Ordering::Relaxed);
    let client = BootstrapClient::new(ChannelTransport::new(handle));

    for _ in 0..SEND_RETRY_LIMIT {
        match client.log(message) {
            Err(IpcError::WouldBlock) => core::hint::spin_loop(),
            _ => return,
        }
    }
}

/// Exit-делегат harness'а: дофлушивает хвост лога и завершает поток с `code`.
fn runner_exit(code: u32) -> ! {
    LOG_WRITER.flush();
    thread_exit(u64::from(code))
}

/// `bootstrap_handle` приходит в x0 как сырой HandleId WRITE-конца канала,
/// переданного ядром при спавне.
#[unsafe(no_mangle)]
pub extern "C" fn _start(bootstrap_handle: usize) -> ! {
    BOOTSTRAP_HANDLE.store(bootstrap_handle, Ordering::Relaxed);
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
