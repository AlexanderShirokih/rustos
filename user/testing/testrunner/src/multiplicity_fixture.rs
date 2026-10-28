#![no_std]
#![no_main]

use bootstrap_abi::image;
use component_startup::{
    AsyncStartup, Attributes, Requirements as _, SessionError, StartupError, StartupFailure, all,
    component_manifest, session_async as request_session,
};
use executor::{Executor, PortChannel};
use ipc::CallError;
use rtc_abi::rtc;
use runtime::{Timeout, log_fmt, log_line};
use syscall::{Handle, kind};

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
    let executor = Executor::new().map_err(|_| Failed)?;

    executor.block_on(async {
        match requirements.rtc.open_async().await {
            Err(StartupError {
                requirement: "rtc",
                failure: StartupFailure::Unbound,
            }) => log_line("optional empty"),
            _ => return Err(Failed),
        }

        let one = match requirements.one.open_async().await {
            Ok(one) => Some(one),
            Err(StartupError {
                failure: StartupFailure::Unbound,
                ..
            }) => None,
            Err(_) => return Err(Failed),
        };

        log_line("started");

        let mut count: u16 = 0;
        let mut providers = requirements
            .probes
            .all(Attributes::empty())
            .await
            .map_err(|_| Failed)?;

        while let Some(found) = providers.next().await {
            let session = requirements
                .probes
                .open(&found.map_err(|_| Failed)?)
                .await
                .map_err(|_| Failed)?;
            let client = PortChannel::client(session);
            let value = u64::from(count);

            if client.ping(value).await != Ok(value) {
                return Err(Failed);
            }
            count = count.checked_add(1).ok_or(Failed)?;
        }

        if (count == 0) != one.is_none() {
            return Err(Failed);
        }

        log_fmt(format_args!("providers {count}"));

        let requests = AsyncStartup::new(PortChannel::new(parent)).with_wait(SESSION_PING_TIMEOUT);

        match all::<probe::Protocol>(&requests, "one", Attributes::empty()).await {
            Err(CallError::Remote(SessionError::Refused)) => log_line("all refused"),
            _ => return Err(Failed),
        }

        let image = requirements
            .images
            .open_first(Attributes::empty())
            .await
            .map_err(|_| Failed)?
            .ok_or(Failed)?;
        let image = PortChannel::client(image);
        match image.region().await {
            Ok(_) => log_line("image found"),
            _ => return Err(Failed),
        }

        if count == 0 {
            await_provider(&requirements, parent).await?;
        }

        core::future::pending::<Result<(), Failed>>().await
    })
}

async fn await_provider(
    requirements: &MultiplicityStartup,
    parent: Handle<kind::Port>,
) -> Result<(), Failed> {
    loop {
        let mut providers = requirements
            .probes
            .all(Attributes::empty())
            .await
            .map_err(|_| Failed)?;

        let found = providers.next().await;

        match found {
            Some(Ok(found)) => {
                let session = requirements.probes.open(&found).await.map_err(|_| Failed)?;

                if PortChannel::client(session).ping(7).await != Ok(7) {
                    return Err(Failed);
                }

                let one = request_session::<probe::Protocol>(
                    parent,
                    "one",
                    Attributes::empty(),
                    SESSION_PING_TIMEOUT,
                )
                .await
                .map_err(|_| Failed)?;

                if PortChannel::client(one).ping(8).await != Ok(8) {
                    return Err(Failed);
                }

                log_line("late provider");
                return Ok(());
            }
            None => {
                executor::sleep(Timeout::from_millis(20)).await;
            }
            Some(Err(_)) => return Err(Failed),
        }
    }
}
