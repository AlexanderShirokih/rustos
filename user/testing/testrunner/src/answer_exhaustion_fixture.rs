#![no_std]
#![no_main]

use kernel_objects::Port;
use runtime::{ExitCode, PortTransport, Timeout};

mod answer_exhaustion_protocol;
mod handle_table_fill;

use answer_exhaustion_protocol::{
    ANSWERED_EXIT_CODE, TABLE_NOT_FULL_EXIT_CODE, answer_exhaustion, exit_code_for,
};
use handle_table_fill::fill_handle_table;

const SERVER_WAIT: Timeout = Timeout::from_secs(5);

#[program_entry::process]
fn run(start: &Port) -> ExitCode {
    let Some(_spare) = fill_handle_table() else {
        return ExitCode::from(TABLE_NOT_FULL_EXIT_CODE);
    };
    let client =
        answer_exhaustion::Client::new(PortTransport::new(start.as_raw())).with_wait(SERVER_WAIT);

    let code = match client.request().wait() {
        Ok(_) => ANSWERED_EXIT_CODE,
        Err(error) => exit_code_for(error),
    };
    ExitCode::from(code)
}
