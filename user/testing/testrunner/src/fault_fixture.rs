//! Компонент выполняет `udf` и завершается исключением, о котором пишет ядро.
#![no_std]
#![no_main]

use syscall::{Handle, kind};

mod failed;

use failed::Failed;

#[program_entry::component]
fn run(_parent: Handle<kind::Port>) -> Result<(), Failed> {
    // SAFETY: исключение от `udf` ядро обрабатывает завершением процесса. Инструкция не трогает
    // память и стек, а код после неё не выполняется.
    #[allow(unsafe_code)]
    unsafe {
        core::arch::asm!("udf #0", options(nomem, nostack));
    }
    Err(Failed)
}
