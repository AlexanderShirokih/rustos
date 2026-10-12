//! Исполняет функции, помеченные `#[test_harness::test]` в модулях этого крейта, и завершает
//! процесс кодом их итога.
//!
//! Набор выбирает переменная сборки `TEST_PROFILE`: `stress` берёт стрессовые прогоны,
//! `bench` - замеры, любое другое значение - обычные тесты. Строки прогона уходят в
//! системный журнал bootstrap-контрактом по стартовому handle процесса.

// Деревья `fdt_test::tree!` тестов раскрываются глубже предела рекурсии по умолчанию.
#![recursion_limit = "256"]
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

extern crate alloc;

use core::{fmt::Write as _, panic::PanicInfo};

use bootstrap_abi::{bootstrap, features, hardware, image, log_message_max, system_info};
use component_startup::component_manifest;
use io::writer::Writer;
use kernel_objects::{Port, Session};
use log_abi::journal;
use rootkeeper::SystemImage;
use runtime::{ExitCode, PortTransport, process_exit};
use spin::{Mutex, Once};
use syscall::{Handle, kind};

mod address_space_isolation;
mod answer_exhaustion_protocol;
mod c_selftest;
mod child_steps_protocol;
mod clock;
mod composition;
mod cxx_exit;
mod cxx_selftest;
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
mod input_hub;
mod ipc_buffer;
mod ipc_raw;
mod irq_provision;
mod journal_fixture_protocol;
mod kernel_log;
mod load_program;
mod log_fixture_protocol;
mod log_relay;
mod memory_capability_target;
mod memory_share;
mod memory_stress;
mod mismatched_kind;
mod owned_handle;
mod parent;
mod pins;
mod pmic_gpio;
mod pmic_keys;
mod port;
mod port_liveness;
mod port_transport;
mod probe_protocol;
mod process_handles;
mod process_lifecycle;
mod process_start;
mod received_frame;
mod register_model;
mod resource_exhaustion;
mod resource_split;
mod ring_transport;
mod rootkeeper_system;
mod screen;
mod search_limits;
mod selftest_clock;
mod selftest_run;
mod session_events;
mod signal;
#[cfg(target_arch = "aarch64")]
mod simd_state;
mod simplefb;
mod smp_stress;
mod source_fixture;
mod spawn_from_image;
mod spmi;
mod sqlite_selftest;
mod startup_exit_protocol;
mod startup_port;
mod std_rt;
mod std_selftest;
mod stress;
mod sync;
mod system_info_session;
#[cfg(target_arch = "aarch64")]
mod thread_pointer;
mod thread_stack;
mod thread_stack_protocol;
mod thread_yield;
mod virtio_input;
mod virtio_mmio;
mod vm;
mod watched_capability;

component_manifest! {
    exports: [
        hardware::Protocol,
        system_info::Protocol,
        image::Protocol,
        features::Protocol,
        journal::Protocol,
    ],
}

static BOOTSTRAP_HANDLE: Once<Handle<kind::Port>> = Once::new();

struct LogBuffer {
    len: usize,
    bytes: [u8; LOG_LINE_MAX],
}

static LOG_BUFFER: Mutex<LogBuffer> = Mutex::new(LogBuffer {
    len: 0,
    bytes: [0; LOG_LINE_MAX],
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
            if state.len == LOG_LINE_MAX {
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
const LOG_LINE_MAX: usize = log_message_max(LOG_TAG);

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
        .expect("bootstrap handle set at entry")
}

pub(crate) fn hardware_session() -> Session<hardware::Protocol> {
    bootstrap::Client::new(PortTransport::new(bootstrap_handle()))
        .acquire_hardware()
        .map(Session::from_session)
        .expect("hardware session granted")
}

pub(crate) fn system_info_session() -> Session<system_info::Protocol> {
    bootstrap::Client::new(PortTransport::new(bootstrap_handle()))
        .acquire_system_info()
        .map(Session::from_session)
        .expect("system info session granted")
}

fn runner_exit(code: u32) -> ! {
    LOG_WRITER.flush();
    log_relay::shutdown_relays();
    process_exit(u64::from(code))
}

#[program_entry::process]
fn run(root: &Port) -> ExitCode {
    BOOTSTRAP_HANDLE.call_once(|| root.as_raw());
    runtime::set_panic_hook(report_panic);
    test_harness::runner::install_writer(&LOG_WRITER);
    test_harness::runner::install_exit(runner_exit);

    let image = SystemImage::acquire(root.as_raw()).expect("userland image is mapped");
    let entries = image.entries().expect("userland image decodes");
    test_harness::run_tests(profile(), Some(&entries))
}

pub(crate) fn profile() -> test_harness::TestKind {
    match env!("TEST_PROFILE") {
        "stress" => test_harness::TestKind::Stress,
        "bench" => test_harness::TestKind::Bench,
        _ => test_harness::TestKind::Regular,
    }
}

#[test_harness::test]
fn userland_smoke() {
    let sum: u64 = (1..=10).sum();
    test_harness::check_eq!(sum, 55);
    test_harness::check!(sum.is_multiple_of(5));
}

struct PanicBuffer {
    len: usize,
    bytes: [u8; LOG_LINE_MAX],
}

impl core::fmt::Write for PanicBuffer {
    fn write_str(&mut self, s: &str) -> core::fmt::Result {
        let take = s.len().min(LOG_LINE_MAX - self.len);
        self.bytes[self.len..self.len + take].copy_from_slice(&s.as_bytes()[..take]);
        self.len += take;
        Ok(())
    }
}

fn report_panic(info: &PanicInfo<'_>) {
    let mut buf = PanicBuffer {
        len: 0,
        bytes: [0; LOG_LINE_MAX],
    };
    let _ = write!(buf, "[TEST-PANIC] {info}");
    send_log_frame(&buf.bytes[..buf.len]);
    log_relay::shutdown_relays();
}
