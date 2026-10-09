#![no_std]
#![no_main]

use component_startup::{Attributes, LOG_REQUIREMENT, STARTUP_REQUEST_TIMEOUT, session};
use log_abi::log;
use runtime::log_fmt;
use syscall::{Handle, Rights, kind};

mod failed;

use failed::Failed;

#[program_entry::component]
fn run(parent: Handle<kind::Port>) -> Result<(), Failed> {
    // Повторный запрос журнала получает копию той же выдачи, что поставила точка входа.
    let log = session::<log::Protocol>(
        parent,
        LOG_REQUIREMENT,
        Attributes::empty(),
        STARTUP_REQUEST_TIMEOUT,
    )
    .map_err(|_| Failed)?;
    let duplicate = log.handle().duplicate(Rights::WRITE, None).map(drop);
    log_fmt(format_args!("{duplicate:?}"));
    Err(Failed)
}
