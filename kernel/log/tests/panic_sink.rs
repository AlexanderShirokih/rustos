//! Путь паники печатает причину при занятом локе `STDOUT`.
//!
//! Статики `klog` общие на весь тестовый бинарь: тесты сериализуются через
//! `SERIAL`, а сток паники регистрируется один раз.

mod common;

use std::{
    sync::{Arc, Mutex, MutexGuard, OnceLock, PoisonError, mpsc},
    thread,
    time::Duration,
};

use common::Recorder;
use io::writer::Writer;

struct ReentrantWriter;

impl Writer for ReentrantWriter {
    fn write_all(&self, _buf: &[u8]) {
        // Панический вывод изнутри записи: модель паники в драйвере консоли,
        // когда лок `STDOUT` уже захвачен `printf`.
        klog::panic_printf(format_args!("reason-under-lock"));
    }

    fn flush(&self) {}
}

static REENTRANT: ReentrantWriter = ReentrantWriter;

static SERIAL: Mutex<()> = Mutex::new(());

fn serialized() -> MutexGuard<'static, ()> {
    SERIAL.lock().unwrap_or_else(PoisonError::into_inner)
}

fn panic_sink() -> &'static Arc<Recorder> {
    static SINK: OnceLock<Arc<Recorder>> = OnceLock::new();
    SINK.get_or_init(|| {
        let sink = Arc::new(Recorder::default());
        klog::set_panic_stdout(sink.clone());
        sink
    })
}

#[test]
fn panic_printf_reaches_sink_while_stdout_lock_is_held() {
    let _serial = serialized();
    let sink = panic_sink();
    klog::set_stdout(&REENTRANT);

    let (done_tx, done_rx) = mpsc::channel();
    thread::spawn(move || {
        klog::printf(format_args!("log line interrupted by panic"));
        let _ = done_tx.send(());
    });

    done_rx
        .recv_timeout(Duration::from_secs(10))
        .expect("printf hung: the panic path waited for the STDOUT lock");
    assert!(sink.panic_contents().contains("reason-under-lock"));
    assert!(!sink.normal_contents().contains("reason-under-lock"));
}

#[test]
fn panic_logf_writes_fatal_prefix_and_line_ending() {
    let _serial = serialized();
    let sink = panic_sink();
    let before = sink.panic_contents().len();

    klog::panic_logf(format_args!("logf-marker"));

    assert_eq!(&sink.panic_contents()[before..], "[FATAL] logf-marker\r\n");
}
