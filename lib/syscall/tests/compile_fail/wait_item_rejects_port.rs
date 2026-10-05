use core::num::NonZeroU32;

use syscall::{Handle, WaitItem, kind};

fn main() {
    let port = Handle::<kind::Port>::from_abi(NonZeroU32::new(3).unwrap());
    let _ = WaitItem::new(port, 1);
}
