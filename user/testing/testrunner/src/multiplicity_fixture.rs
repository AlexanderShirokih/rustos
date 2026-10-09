#![no_std]
#![no_main]

use bootstrap_abi::image;
use component_startup::{
    Attributes, Requirements as _, SessionError, StartupError, StartupFailure, all,
    component_manifest, session as request_session, start_parent,
};
use ipc::CallError;
use kernel_objects::{Port, Signal};
use rtc_abi::rtc;
use runtime::{Timeout, log_fmt, log_line};
use syscall::{Handle, SIGNALED, kind};

mod failed;
mod probe_protocol;

use failed::Failed;
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

#[program_entry::component]
fn run(parent: Handle<kind::Port>) -> Result<(), Failed> {
    let requirements = MultiplicityStartup::from_parent(parent);

    match requirements.rtc.open() {
        Err(StartupError {
            requirement: "rtc",
            failure: StartupFailure::Unbound,
        }) => log_line("optional empty"),
        _ => return Err(Failed),
    }
    let one = match requirements.one.open() {
        Ok(one) => Some(one),
        Err(StartupError {
            failure: StartupFailure::Unbound,
            ..
        }) => None,
        Err(_) => return Err(Failed),
    };
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
    if (count == 0) != one.is_none() {
        return Err(Failed);
    }
    log_fmt(format_args!("providers {count}"));

    let requests = start_parent(parent).with_wait(SESSION_PING_TIMEOUT);
    match all::<probe::Protocol>(&requests, "one", Attributes::empty()) {
        Err(CallError::Remote(SessionError::Refused)) => log_line("all refused"),
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
        Ok(_) => log_line("image found"),
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

                    let one = request_session::<probe::Protocol>(
                        parent,
                        "one",
                        Attributes::empty(),
                        SESSION_PING_TIMEOUT,
                    )
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
