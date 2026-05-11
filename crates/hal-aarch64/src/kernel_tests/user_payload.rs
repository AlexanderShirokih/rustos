use syscall::SyscallOp;
pub use test_harness_qemu_aarch64::payload::*;

pub const fn svc_op(op: SyscallOp) -> Instruction {
    svc(op as u16)
}
