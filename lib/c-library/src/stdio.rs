//! Стандартные потоки библиотеки C: дескрипторы 1 и 2 пишут в журнал процесса, дескриптор 0
//! сразу отдаёт конец данных.
//!
//! Байты вывода собираются в текст UTF-8, и журнал получает строки потока целиком.
//! Незавершённые строки и последовательности выталкивает [`flush`].

use core::ffi::c_int;

use log_abi::Utf8Assembler;
use runtime::{Mutex, log::Stream};

use crate::errno::Errno;

const STDIN: c_int = 0;

const STDOUT: c_int = 1;

const STDERR: c_int = 2;

static STDOUT_STREAM: Output = Output::new();

static STDERR_STREAM: Output = Output::new();

struct Output {
    text: Mutex<Utf8Assembler>,
    lines: Stream,
}

impl Output {
    const fn new() -> Self {
        Self {
            text: Mutex::new(Utf8Assembler::new()),
            lines: Stream::new(),
        }
    }

    fn write(&self, bytes: &[u8]) -> Result<(), Errno> {
        let mut failed = false;
        self.text.lock().push(bytes, |text| {
            failed |= self.lines.write(text).is_err();
        });
        if failed { Err(Errno::Io) } else { Ok(()) }
    }

    fn flush(&self) {
        self.text.lock().finish(|text| {
            let _ = self.lines.write(text);
        });
        let _ = self.lines.flush();
    }
}

pub(crate) fn flush() {
    STDERR_STREAM.flush();
    STDOUT_STREAM.flush();
}

fn output(fd: c_int) -> Option<&'static Output> {
    match fd {
        STDOUT => Some(&STDOUT_STREAM),
        STDERR => Some(&STDERR_STREAM),
        _ => None,
    }
}

pub(crate) fn is_standard(fd: c_int) -> bool {
    matches!(fd, STDIN | STDOUT | STDERR)
}

pub(crate) fn write(fd: c_int, bytes: &[u8]) -> Result<usize, Errno> {
    let output = output(fd).ok_or(Errno::BadDescriptor)?;
    // Байты незавершённой последовательности остаются в потоке и тоже считаются записанными.
    output.write(bytes)?;
    Ok(bytes.len())
}

pub(crate) fn read(fd: c_int) -> Result<usize, Errno> {
    if fd == STDIN {
        Ok(0)
    } else {
        Err(Errno::BadDescriptor)
    }
}
