//! E2E полной цепочки userland: initrd-blob -> production-путь
//! `kernelspace::bootstrap::spawn_process` -> `Bootstrap::log` из EL0 ->
//! teardown по PEER_CLOSED с ожиданием THREAD_TERMINATED.

use bootstrap::{BootstrapService, dispatch_bootstrap};
use ipc::wire::Str;
use kernel_tests::kernel_test;
use kernelspace::bootstrap::{BootstrapLaunch, ChannelTransport};
use kobject::{
    CHANNEL_PEER_CLOSED, CHANNEL_READABLE, Handle, HandleId, KObject, Rights, THREAD_TERMINATED,
    handle_close, install_handle, object_wait_one,
};
use scheduler::ArchContext;

use crate::sched::Aarch64Context;

/// Bounded-бюджет ожидания как детектор провала: хэппи-пас
/// будится сигналом за миллисекунды.
const WAIT_BUDGET_NS: u64 = 5_000_000_000;

fn spawn_bootstrap_with_log() -> (HandleId, BootstrapLaunch) {
    let blob = kernelspace::kernel_tests::userland_blob()
        .expect("userland blob must be present in test mode");

    let launch = kernelspace::bootstrap::spawn_process(
        kernelspace::kernel_tests::user_process_launcher().as_ref(),
        blob,
        Aarch64Context::USER_VA_END,
    )
    .expect("spawn_process must succeed");
    kernel_tests::kassert_eq!(launch.info.initial_handle_ids.len(), 1);

    let chan_id = install_handle(Handle::new(
        KObject::Channel(launch.channel.clone()),
        Rights::READ | Rights::WAIT | Rights::INSPECT,
    ))
    .expect("install local channel handle");

    let observed = object_wait_one(
        chan_id,
        CHANNEL_READABLE | CHANNEL_PEER_CLOSED,
        Some(WAIT_BUDGET_NS),
    )
    .expect("wait for bootstrap log");
    kernel_tests::kassert!(observed & CHANNEL_READABLE != 0);

    let transport = ChannelTransport::new(chan_id);
    let mut probe = LogProbe { received: false };
    dispatch_bootstrap(&mut probe, &transport).expect("bootstrap log must dispatch");
    kernel_tests::kassert!(probe.received);

    (chan_id, launch)
}

struct LogProbe {
    received: bool,
}

impl BootstrapService for LogProbe {
    fn log(&mut self, message: Str<{ bootstrap::LOG_MESSAGE_MAX }>) {
        self.received = message.as_str() == "rootkeeper started";
    }
}

// Закрытие kernel-конца поднимает PEER_CLOSED и сигнально будит bootstrap
// из сна;
fn teardown_bootstrap(chan_id: HandleId, launch: BootstrapLaunch) {
    handle_close(chan_id).expect("close local channel handle");
    drop(launch.channel);

    let thread_id = install_handle(Handle::new(
        KObject::Thread(launch.info.thread_object),
        Rights::WAIT | Rights::INSPECT,
    ))
    .expect("install bootstrap thread handle");
    let observed = object_wait_one(thread_id, THREAD_TERMINATED, Some(WAIT_BUDGET_NS))
        .expect("wait for bootstrap exit");
    kernel_tests::kassert!(observed & THREAD_TERMINATED != 0);
}

#[kernel_test]
fn userland_bootstrap_log() {
    let (chan_id, launch) = spawn_bootstrap_with_log();
    teardown_bootstrap(chan_id, launch);
}
