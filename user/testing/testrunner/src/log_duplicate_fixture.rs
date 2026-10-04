#![no_std]
#![no_main]
#![allow(unsafe_code)]

use component_startup::{Attributes, LOG_REQUIREMENT, component_startup::Client, session};
use kernel_objects::{Port, PortTransport};
use log_abi::log;
use runtime::{install_log, log_fmt};
use syscall::Rights;

mod fixture_start;

use fixture_start::{Failed, fixture_start};

const LOG_TAG: &str = "duplicate";

fixture_start!(run);

fn run(parent: &Port) -> Result<(), Failed> {
    let requests = Client::new(PortTransport::new(parent));
    let log = session::<log::Protocol>(&requests, LOG_REQUIREMENT, Attributes::empty())
        .map_err(|_| Failed)?
        .map_err(|_| Failed)?;
    let duplicate = log.handle().duplicate(Rights::WRITE, None).map(drop);
    install_log(log.into_handle().into_issued(), LOG_TAG);
    log_fmt(format_args!("{duplicate:?}"));
    Err(Failed)
}
