#![no_std]
#![no_main]

extern crate alloc;

use alloc::rc::Rc;
use core::fmt::Debug;

use bootstrap_abi::{features, hardware, image, system_info};
use component_startup::{
    Attributes, Providers, Requirements as _, SessionPort, component_manifest,
};
use executor::{Executor, PortChannel};
use kernel_objects::Session;
use log_abi::journal;
use runtime::{log_fmt, log_line};
use syscall::{Handle, kind};

mod failed;
mod probe_protocol;

use failed::Failed;
use probe_protocol::{probe, serve_probe};

component_manifest! {
    struct FeaturedExportStartup {
        requirements: {
            hardware: all(hardware::Protocol),
            system_info: all(system_info::Protocol),
            images: all(image::Protocol),
            features: all(features::Protocol),
            journal: all(journal::Protocol),
        },
        features: ["testrunner,featured"],
        exports: [probe::Protocol => [{ kind = "featured" }]],
    }
}

#[program_entry::component]
fn run(parent: Handle<kind::Port>) -> Result<(), Failed> {
    let requirements = FeaturedExportStartup::from_parent(parent);
    let executor = Rc::new(Executor::new().map_err(|_| Failed)?);

    executor.block_on(async {
        log_opened("hardware", &requirements.hardware, async |_session| {}).await?;
        log_opened("system_info", &requirements.system_info, async |session| {
            PortChannel::client(session)
                .snapshot()
                .await
                .is_ok_and(|snapshot| {
                    snapshot.cpus > 0 && snapshot.free_frames <= snapshot.managed_frames
                })
        })
        .await?;

        log_opened("image", &requirements.images, async |_session| {}).await?;
        log_opened("features", &requirements.features, async |_session| {}).await?;
        log_opened("journal", &requirements.journal, async |_session| {}).await?;

        let export = SessionPort::create().map_err(|_| Failed)?;
        log_line("started");
        serve_probe(&executor, parent, export).await
    })
}

async fn log_opened<P: ipc::Protocol, T: Debug>(
    name: &str,
    providers: &Providers<P>,
    inspect: impl AsyncFnOnce(Session<P>) -> T,
) -> Result<(), Failed> {
    let mut found = providers
        .all(Attributes::empty())
        .await
        .map_err(|_| Failed)?;

    let found = found.next().await.ok_or(Failed)?.map_err(|_| Failed)?;

    let opened = match providers.open(&found).await {
        Ok(session) => Ok(inspect(session).await),
        Err(error) => Err(error.failure),
    };

    log_fmt(format_args!("{name} {opened:?}"));
    Ok(())
}
