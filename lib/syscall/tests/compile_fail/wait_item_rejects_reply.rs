use core::num::NonZeroU32;

use syscall::{Handle, WaitItem, kind};

fn main() {
    let reply = Handle::<kind::Reply>::from_abi(NonZeroU32::new(3).unwrap());
    let _ = WaitItem::new(reply, 1);
}
