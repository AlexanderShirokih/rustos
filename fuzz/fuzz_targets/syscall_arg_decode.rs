#![no_main]

use libfuzzer_sys::fuzz_target;
use syscall::{
    Handle, MemoryProfile, Rights, SyscallError, SyscallOp, Timeout, UserMemFlags, WaitItem,
    decode_image_desc, decode_segment, decode_tag, encode_image_desc, encode_segment, encode_tag,
};

const ARG_COUNT: usize = 6;

fuzz_target!(|data: &[u8]| {
    let mut rest = data;

    if let Some(raw) = take(&mut rest, 2) {
        let _ = SyscallOp::from_raw(u16::from_le_bytes([raw[0], raw[1]]));
    }

    for _ in 0..ARG_COUNT {
        let Some(bytes) = take(&mut rest, 8) else {
            break;
        };
        let Ok(bytes) = <[u8; 8]>::try_from(bytes) else {
            break;
        };
        decode_arg(u64::from_le_bytes(bytes));
    }

    if let Some(desc) = decode_image_desc(rest) {
        let _ = encode_image_desc(&desc);
    }
    if let Some(segment) = decode_segment(rest) {
        let _ = encode_segment(&segment);
    }
});

fn decode_arg(arg: u64) {
    let _ = UserMemFlags::from_raw(arg);
    let _ = MemoryProfile::from_raw(arg);
    let _ = Timeout::from_ns(arg).raw();

    let tag = decode_tag(arg);
    let _ = encode_tag(tag.len, tag.ncaps);

    let low = arg as u32;
    let signed = arg.cast_signed();
    let _ = Rights::from_bits_truncate(low);
    let _ = SyscallError::from_code(low);
    let _ = SyscallError::from_return(signed);
    let _ = Handle::from_syscall_return(signed);
    if let Some(handle) = Handle::new(low) {
        let _ = WaitItem::new(handle, (arg >> 32) as u32);
    }
}

fn take<'a>(rest: &mut &'a [u8], len: usize) -> Option<&'a [u8]> {
    let (head, tail) = rest.split_at_checked(len)?;
    *rest = tail;
    Some(head)
}
