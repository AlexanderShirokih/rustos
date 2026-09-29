use kernel_objects::Signal;

pub fn fill_handle_table() -> Option<Signal> {
    let mut last = None;
    while let Ok(signal) = Signal::create() {
        if let Some(previous) = last.replace(signal) {
            let _ = previous.into_raw();
        }
    }
    last
}
