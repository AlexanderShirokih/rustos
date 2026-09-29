use kernel_objects::Signal;
use syscall::{Handle, kind};

pub fn fill_handle_table() -> Option<Handle<kind::Signal>> {
    let mut last = None;
    while let Ok(signal) = Signal::create() {
        last = Some(signal.into_raw());
    }
    last
}
