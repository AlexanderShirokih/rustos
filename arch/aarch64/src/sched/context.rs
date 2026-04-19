use core::{mem, ptr::NonNull};

use kernel::sched::ArchContext;

use super::{cpu_local::Aarch64Cpu, stack::Aarch64Stack, switch::context_switch};
use super::switch::context_start;

#[repr(C)]
pub struct Aarch64Context {
    pub x19_x28: [u64; 10],
    pub fp: u64,
    pub lr: u64,
    pub sp: u64,
    pub pstate: u64,
}

const _: () = assert!(mem::size_of::<Aarch64Context>().is_multiple_of(16));

impl ArchContext for Aarch64Context {
    type Cpu = Aarch64Cpu;
    type Stack = Aarch64Stack;

    fn init(
        stack_top: NonNull<u8>,
        entry: kernel::sched::arch::TrampolineFn,
        arg: *mut (),
    ) -> Self {
        let sp = (stack_top.as_ptr() as usize & !0xF) as u64;
        let mut context = Self {
            x19_x28: [0; 10],
            fp: 0,
            lr: thread_entry_shim as *const () as usize as u64,
            sp,
            pstate: 0,
        };
        context.x19_x28[0] = arg as usize as u64;
        context.x19_x28[1] = entry as usize as u64;
        context
    }

    unsafe fn start(next: &Self) -> ! {
        // SAFETY: используется только для первого входа в поток из boot context.
        unsafe { context_start(next as *const Self) }
    }

    unsafe fn switch(prev: &mut Self, next: &Self) {
        // SAFETY: вызывается scheduler при эксклюзивном владении обоими контекстами.
        unsafe { context_switch(prev as *mut Self, next as *const Self) };
    }
}

#[unsafe(naked)]
unsafe extern "C" fn thread_entry_shim() -> ! {
    core::arch::naked_asm!(
        "mov x0, x19",
        "br x20",
    )
}
