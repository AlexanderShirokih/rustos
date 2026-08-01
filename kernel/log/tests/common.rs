#![allow(dead_code)]

use std::sync::{Mutex, PoisonError};

use io::writer::Writer;

/// Writer-протоколист: копит нормальный и панический каналы раздельно,
/// фиксируя, какой стороной writer'а шла запись.
#[derive(Default)]
pub struct Recorder {
    normal: Mutex<String>,
    panic: Mutex<String>,
}

impl Recorder {
    pub fn normal_contents(&self) -> String {
        self.normal
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .clone()
    }

    pub fn panic_contents(&self) -> String {
        self.panic
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .clone()
    }
}

impl Writer for Recorder {
    fn write_all(&self, buf: &[u8]) {
        self.normal
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .push_str(&String::from_utf8_lossy(buf));
    }

    fn write_all_panic(&self, buf: &[u8]) {
        self.panic
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .push_str(&String::from_utf8_lossy(buf));
    }

    fn flush(&self) {}
}
