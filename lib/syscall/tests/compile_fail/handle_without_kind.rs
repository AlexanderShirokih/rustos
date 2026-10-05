use core::num::NonZeroU32;

use syscall::Handle;

fn main() {
    let _: Handle = Handle::from_abi(NonZeroU32::new(7).unwrap());
}
