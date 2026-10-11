//! Подключение источников: наблюдение за экспортами `InputSource` и передача каждому выданному
//! экспорту новой сессии `InputInject`.

use alloc::vec::Vec;
use core::time::Duration;

use component_startup::{
    Attributes, Found, IpcError, Providers, Session, SessionPort, StartupError, StartupFailure,
};
use input_abi::input_source;
use kernel_objects::SessionTransport;
use runtime::{log_fmt, sleep};
use syscall::Timeout;

use crate::hub::open_source;

const RETRY_INTERVAL: Timeout = Timeout::from_secs(1);

const NO_SPACE_PAUSE: Duration = Duration::from_secs(1);

const ATTACH_TIMEOUT: Timeout = Timeout::from_secs(2);

type Attached = input_source::Client<SessionTransport>;

pub(crate) fn attach_sources(
    providers: &Providers<input_source::Protocol>,
    sources: &SessionPort,
) -> StartupError {
    let watch = match providers.watch(Attributes::empty()) {
        Ok(watch) => watch,
        Err(error) => return error,
    };
    let mut late = Vec::new();
    let mut attached = Vec::new();

    loop {
        let wait = if late.is_empty() {
            Timeout::INFINITE
        } else {
            RETRY_INTERVAL
        };

        match watch.next(wait) {
            Ok(found) => attach(providers, sources, found, &mut late, &mut attached),
            Err(error) if timed_out(&error) => {
                for found in core::mem::take(&mut late) {
                    attach(providers, sources, found, &mut late, &mut attached);
                }
            }
            Err(error) if error.failure == StartupFailure::NoSpace => sleep(NO_SPACE_PAUSE),
            Err(error) => return error,
        }
    }
}

fn attach(
    providers: &Providers<input_source::Protocol>,
    sources: &SessionPort,
    found: Found,
    late: &mut Vec<Found>,
    attached: &mut Vec<Attached>,
) {
    let source = match providers.open(&found) {
        Ok(source) => source.client().with_wait(ATTACH_TIMEOUT),
        Err(error) if timed_out(&error) => {
            late.push(found);
            return;
        }
        Err(error) => {
            log_fmt(format_args!("source not opened: {error}"));
            return;
        }
    };

    let inject = match open_source(sources) {
        Ok(inject) => Session::from_handle(inject),
        Err(error) => {
            log_fmt(format_args!("source session not issued: {error:?}"));
            return;
        }
    };

    match source.attach(inject) {
        Ok(()) => attached.push(source),
        Err(error) => log_fmt(format_args!("source attach failed: {error:?}")),
    }
}

fn timed_out(error: &StartupError) -> bool {
    error.failure == StartupFailure::Ipc(IpcError::Timeout)
}
