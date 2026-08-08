#![no_std]
#![no_main]
#![allow(unsafe_code)]

extern crate alloc;

use core::{fmt::Write as _, panic::PanicInfo};

use bootstrap::{BootstrapClient, LOG_MESSAGE_MAX};
use io::writer::Writer;
use ipc::wire::IpcError;
use kernel_tests::kernel_test;
use runtime::{PortTransport, thread_exit};
use spin::{Mutex, Once};
use syscall::Handle;

mod address_space_isolation;
mod clock;
mod filesystem;
mod filesystem_scope;
mod filesystem_stress;
mod fixture;
mod fixture_protocol;
mod heap;
mod ipc_buffer;
mod ipc_raw;
mod irq_provision;
mod load_entry;
mod log_fixture_protocol;
mod log_relay;
mod managed_fixture_protocol;
mod managed_startup;
mod memory_capability_target;
mod memory_share;
mod owned_handle;
mod port;
mod port_liveness;
mod port_transport;
mod process_handles;
mod process_lifecycle;
mod process_start;
mod resource_split;
mod ring_transport;
mod rootkeeper_system;
mod signal;
mod simd_state;
mod smp_stress;
mod spawn_from_image;
mod startup_fixture_protocol;
mod stress;
mod sync;
mod thread_pointer;
mod thread_yield;
mod vm;

static BOOTSTRAP_HANDLE: Once<Handle> = Once::new();

struct LogBuffer {
    len: usize,
    bytes: [u8; LOG_MESSAGE_MAX],
}

static LOG_BUFFER: Mutex<LogBuffer> = Mutex::new(LogBuffer {
    len: 0,
    bytes: [0; LOG_MESSAGE_MAX],
});

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

fn flush_line(state: &mut LogBuffer) {
    if state.len == 0 {
        return;
    }
    send_log_frame(&state.bytes[..state.len]);
    state.len = 0;
}

const LOG_TAG: &str = "testrunner";

fn send_log_frame(payload: &[u8]) {
    const SEND_RETRY_LIMIT: usize = 1024;

    let Ok(text) = core::str::from_utf8(payload) else {
        return;
    };

    let Some(&handle) = BOOTSTRAP_HANDLE.get() else {
        return;
    };
    let client = BootstrapClient::new(PortTransport::client(handle));

    for _ in 0..SEND_RETRY_LIMIT {
        match client.log_str(LOG_TAG, text) {
            Err(IpcError::WouldBlock) => core::hint::spin_loop(),
            _ => return,
        }
    }
}

pub(crate) fn bootstrap_handle() -> Handle {
    *BOOTSTRAP_HANDLE
        .get()
        .expect("bootstrap handle set in _start")
}

fn runner_exit(code: u32) -> ! {
    LOG_WRITER.flush();
    log_relay::shutdown_relays();
    thread_exit(u64::from(code))
}

/// `root` приходит в x0 как HandleId WRITE-конца канала, переданного ядром при спавне.
#[unsafe(no_mangle)]
pub extern "C" fn _start(root: Handle) -> ! {
    BOOTSTRAP_HANDLE.call_once(|| root);
    kernel_tests::runner::install_writer(&LOG_WRITER);
    kernel_tests::runner::install_exit(runner_exit);
    let profile = match env!("TEST_PROFILE") {
        "stress" => kernel_tests::TestKind::Stress,
        _ => kernel_tests::TestKind::Regular,
    };
    kernel_tests::run_tests(profile)
}

#[kernel_test]
fn userland_smoke() {
    let sum: u64 = (1..=10).sum();
    kernel_tests::kassert_eq!(sum, 55);
    kernel_tests::kassert!(sum.is_multiple_of(5));
}

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
    log_relay::shutdown_relays();
    thread_exit(1)
}
