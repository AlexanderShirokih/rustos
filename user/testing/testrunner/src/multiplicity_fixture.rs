#![no_std]
#![no_main]
#![allow(unsafe_code)]

use bootstrap_abi::image;
use component_startup::{
    Attributes, Requirements as _, SessionError, all, component_manifest,
    session as request_session, start_parent,
};
use kernel_objects::{Port, Signal};
use rtc_abi::rtc;
use runtime::{Timeout, log_fmt, log_line, start_component};
use syscall::SIGNALED;

mod fixture_start;
mod probe_protocol;

use fixture_start::{Failed, fixture_start};
use probe_protocol::probe;

const SESSION_PING_TIMEOUT: Timeout = Timeout::from_secs(2);

component_manifest! {
    struct MultiplicityStartup {
        requirements: {
            rtc: session(rtc::Protocol),
            probes: all(probe::Protocol),
            one: session(probe::Protocol),
            images: all(image::Protocol),
        },
    }
}

fixture_start!(run);

fn run(parent: &Port) -> Result<(), Failed> {
    start_component(parent.as_raw(), "multiplicity");
    let requirements = MultiplicityStartup::request(parent.as_raw()).map_err(|_| Failed)?;

    if requirements.rtc.is_some() {
        return Err(Failed);
    }
    log_line("optional empty");
    log_line("started");

    let mut count: u16 = 0;
    for found in requirements
        .probes
        .all(Attributes::empty())
        .map_err(|_| Failed)?
    {
        let session = requirements
            .probes
            .open(&found.map_err(|_| Failed)?)
            .map_err(|_| Failed)?;
        let value = u64::from(count);

        if session.client().with_wait(SESSION_PING_TIMEOUT).ping(value) != Ok(value) {
            return Err(Failed);
        }
        count = count.checked_add(1).ok_or(Failed)?;
    }
    if (count == 0) != requirements.one.is_none() {
        return Err(Failed);
    }
    log_fmt(format_args!("providers {count}"));

    let requests = start_parent(parent.as_raw()).with_wait(SESSION_PING_TIMEOUT);
    match all::<probe::Protocol>(&requests, "one", Attributes::empty()) {
        Ok(Err(SessionError::Refused)) => log_line("all refused"),
        _ => return Err(Failed),
    }

    let image = requirements
        .images
        .all(Attributes::empty())
        .map_err(|_| Failed)?
        .next()
        .ok_or(Failed)?
        .map_err(|_| Failed)?;
    let image = requirements.images.open(&image).map_err(|_| Failed)?;
    match image.client().with_wait(SESSION_PING_TIMEOUT).region() {
        Ok(Ok(_)) => log_line("image found"),
        _ => return Err(Failed),
    }

    if count == 0 {
        let timer = Signal::create().map_err(|_| Failed)?;
        loop {
            let found = requirements
                .probes
                .all(Attributes::empty())
                .map_err(|_| Failed)?
                .next();
            match found {
                Some(Ok(found)) => {
                    let session = requirements.probes.open(&found).map_err(|_| Failed)?;
                    if session.client().with_wait(SESSION_PING_TIMEOUT).ping(7) != Ok(7) {
                        return Err(Failed);
                    }

                    let one =
                        request_session::<probe::Protocol>(&requests, "one", Attributes::empty())
                            .map_err(|_| Failed)?
                            .map_err(|_| Failed)?;
                    if one.client().with_wait(SESSION_PING_TIMEOUT).ping(8) != Ok(8) {
                        return Err(Failed);
                    }
                    log_line("late provider");
                    break;
                }
                None => {
                    let _ = timer.wait(SIGNALED, Timeout::from_millis(20));
                }
                Some(Err(_)) => return Err(Failed),
            }
        }
    }

    let idle = Port::create().map_err(|_| Failed)?;
    let _ = idle.recv(Timeout::INFINITE);
    Err(Failed)
}
