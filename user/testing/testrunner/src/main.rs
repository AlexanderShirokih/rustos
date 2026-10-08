//! Исполняет функции, помеченные `#[test_harness::test]` в модулях этого крейта, и завершает
//! процесс кодом их итога.
//!
//! Набор выбирает переменная сборки `TEST_PROFILE`: `stress` берёт стрессовые прогоны,
//! `bench` - замеры, любое другое значение - обычные тесты. Строки прогона уходят в
//! системный журнал bootstrap-контрактом по handle, который программа получает аргументом
//! точки входа `_start`.

#![no_std]
#![no_main]
#![allow(unsafe_code)]
#![allow(
    clippy::expect_used,
    clippy::indexing_slicing,
    clippy::missing_asserts_for_indexing,
    clippy::cast_possible_truncation,
    clippy::unwrap_in_result,
    clippy::multiple_unsafe_ops_per_block,
    clippy::panic
)]

runtime::process_heap!();

extern crate alloc;

use core::{fmt::Write as _, panic::PanicInfo};

use bootstrap_abi::{LOG_MESSAGE_MAX, bootstrap, features, hardware, image};
use component_startup::component_manifest;
use io::writer::Writer;
use kernel_objects::Session;
use runtime::{PortTransport, process_exit};
use spin::{Mutex, Once};
use syscall::{Handle, kind};

mod address_space_isolation;
mod answer_exhaustion_protocol;
mod c_selftest;
mod child_steps_protocol;
mod clock;
mod composition;
mod display;
mod display_fixture_protocol;
mod failing_filesystem;
mod filesystem;
mod filesystem_bench;
mod filesystem_scope;
mod filesystem_stress;
mod fixture;
mod fixture_protocol;
mod futex;
mod heap;
mod heap_bench;
mod ipc_buffer;
mod ipc_raw;
mod irq_provision;
mod load_program;
mod log_fixture_protocol;
mod log_relay;
mod memory_capability_target;
mod memory_share;
mod mismatched_kind;
mod owned_handle;
mod parent;
mod port;
mod port_liveness;
mod port_transport;
mod probe_protocol;
mod process_handles;
mod process_lifecycle;
mod process_start;
mod received_frame;
mod resource_exhaustion;
mod resource_split;
mod ring_transport;
mod rootkeeper_system;
mod signal;
#[cfg(target_arch = "aarch64")]
mod simd_state;
mod simplefb;
mod smp_stress;
mod spawn_from_image;
mod startup_exit_protocol;
mod startup_port;
mod std_rt;
mod std_selftest;
mod stress;
mod sync;
#[cfg(target_arch = "aarch64")]
mod thread_pointer;
mod thread_stack;
mod thread_stack_protocol;
mod thread_yield;
mod virtio_mmio;
mod vm;
mod watched_capability;

component_manifest! {
    exports: [hardware::Protocol, image::Protocol, features::Protocol],
}

static BOOTSTRAP_HANDLE: Once<Handle<kind::Port>> = Once::new();

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
    let Ok(text) = core::str::from_utf8(payload) else {
        return;
    };

    let Some(&handle) = BOOTSTRAP_HANDLE.get() else {
        return;
    };
    let _ = bootstrap::Client::new(PortTransport::new(handle)).log_str(LOG_TAG, text);
}

pub(crate) fn bootstrap_handle() -> Handle<kind::Port> {
    *BOOTSTRAP_HANDLE
        .get()
        .expect("bootstrap handle set in _start")
}

pub(crate) fn hardware_session() -> Session<hardware::Protocol> {
    bootstrap::Client::new(PortTransport::new(bootstrap_handle()))
        .acquire_hardware()
        .map(Session::from_session)
        .expect("hardware session granted")
}

fn runner_exit(code: u32) -> ! {
    LOG_WRITER.flush();
    log_relay::shutdown_relays();
    process_exit(u64::from(code))
}

/// `root` приходит в x0 как HandleId WRITE-конца канала, переданного ядром при спавне.
#[unsafe(no_mangle)]
pub extern "C" fn _start(root: Handle<kind::Port>) -> ! {
    BOOTSTRAP_HANDLE.call_once(|| root);
    test_harness::runner::install_writer(&LOG_WRITER);
    test_harness::runner::install_exit(runner_exit);
    let profile = match env!("TEST_PROFILE") {
        "stress" => test_harness::TestKind::Stress,
        "bench" => test_harness::TestKind::Bench,
        _ => test_harness::TestKind::Regular,
    };
    test_harness::run_tests(profile)
}

#[test_harness::test]
fn userland_smoke() {
    let sum: u64 = (1..=10).sum();
    test_harness::check_eq!(sum, 55);
    test_harness::check!(sum.is_multiple_of(5));
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
    // Ядро выключает машину по завершении всего процесса, а прогон держит сервисные потоки.
    // Поэтому паника завершает процесс, а не только паникующий поток.
    process_exit(1)
}
