use core::num::NonZeroU32;

use syscall::{Handle, kind};

fn send(port: Handle<kind::Port>) -> Handle<kind::Port> {
    port
}

fn main() {
    let region = Handle::<kind::MemoryRegion>::from_abi(NonZeroU32::new(7).unwrap());
    let _ = send(region);
}
