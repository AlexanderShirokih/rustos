//! Подключение источников: наблюдение за экспортами `InputSource` и передача каждому выданному
//! экспорту новой сессии `InputInject`.

use alloc::vec::Vec;

use component_startup::{
    Attributes, Found, IpcError, Providers, Session, SessionPort, StartupError, StartupFailure,
};
use executor::PortChannel;
use input_abi::input_source;
use runtime::log_fmt;
use syscall::Timeout;

use crate::hub::open_source;

const RETRY_INTERVAL: Timeout = Timeout::from_secs(1);

const NO_SPACE_PAUSE: Timeout = Timeout::from_secs(1);

const ATTACH_TIMEOUT: Timeout = Timeout::from_secs(2);

type Attached = Session<input_source::Protocol>;

pub(crate) async fn attach_sources(
    providers: &Providers<input_source::Protocol>,
    sources: &SessionPort,
) -> StartupError {
    let watch = match providers.watch(Attributes::empty()).await {
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

        match watch.next(wait).await {
            Ok(found) => attach(providers, sources, found, &mut late, &mut attached).await,
            Err(error) if timed_out(&error) => {
                for found in core::mem::take(&mut late) {
                    attach(providers, sources, found, &mut late, &mut attached).await;
                }
            }
            Err(error) if error.failure == StartupFailure::NoSpace => {
                executor::sleep(NO_SPACE_PAUSE).await;
            }
            Err(error) => return error,
        }
    }
}

async fn attach(
    providers: &Providers<input_source::Protocol>,
    sources: &SessionPort,
    found: Found,
    late: &mut Vec<Found>,
    attached: &mut Vec<Attached>,
) {
    let source = match providers.open(&found).await {
        Ok(source) => source,
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

    let client =
        input_source::Client::new(PortChannel::new(source.handle())).with_wait(ATTACH_TIMEOUT);
    match client.attach(inject).await {
        Ok(()) => attached.push(source),
        Err(error) => log_fmt(format_args!("source attach failed: {error:?}")),
    }
}

fn timed_out(error: &StartupError) -> bool {
    error.failure == StartupFailure::Ipc(IpcError::Timeout)
}
