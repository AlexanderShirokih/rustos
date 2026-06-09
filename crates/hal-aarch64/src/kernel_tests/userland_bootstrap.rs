//! E2E полной цепочки userland: initrd-blob -> production-путь
//! `kernelspace::bootstrap::spawn_rootkeeper` -> RKHELLO и RKBEAT из EL0 ->
//! teardown по PEER_CLOSED с ожиданием THREAD_TERMINATED.

use kernel_tests::kernel_test;
use kernelspace::bootstrap::RootkeeperLaunch;
use kobject::{
    CHANNEL_PEER_CLOSED, CHANNEL_READABLE, Handle, HandleId, KObject, Rights, THREAD_TERMINATED,
    channel_read, handle_close, install_handle, object_wait_one,
};
use scheduler::ArchContext;
use userland_abi::{
    BOOTSTRAP_ABI_VERSION, BOOTSTRAP_HEARTBEAT_PERIOD_NS, BOOTSTRAP_HELLO_MAGIC,
    parse_bootstrap_heartbeat, parse_bootstrap_hello,
};

use crate::sched::Aarch64Context;

fn spawn_rootkeeper_with_hello() -> (HandleId, RootkeeperLaunch) {
    let blob = kernelspace::kernel_tests::userland_blob()
        .expect("userland blob must be present in test mode");

    let launch = kernelspace::bootstrap::spawn_rootkeeper(
        kernelspace::kernel_tests::user_process_launcher().as_ref(),
        blob,
        Aarch64Context::USER_VA_END,
    )
    .expect("spawn_rootkeeper must succeed");
    kernel_tests::kassert_eq!(launch.info.initial_handle_ids.len(), 1);

    let chan_id = install_handle(Handle::new(
        KObject::Channel(launch.channel.clone()),
        Rights::READ | Rights::WAIT | Rights::INSPECT,
    ))
    .expect("install local channel handle");

    let observed = object_wait_one(
        chan_id,
        CHANNEL_READABLE | CHANNEL_PEER_CLOSED,
        Some(5 * BOOTSTRAP_HEARTBEAT_PERIOD_NS),
    )
    .expect("wait for rootkeeper hello");
    kernel_tests::kassert!(observed & CHANNEL_READABLE != 0);

    let message = channel_read(chan_id).expect("channel must yield rootkeeper hello");
    let hello = parse_bootstrap_hello(message.bytes()).expect("RKHELLO must parse");
    kernel_tests::kassert_eq!(hello.version, BOOTSTRAP_ABI_VERSION);
    kernel_tests::kassert_eq!(hello.magic, BOOTSTRAP_HELLO_MAGIC);

    (chan_id, launch)
}

// Закрытие kernel-конца поднимает PEER_CLOSED и сигнально будит rootkeeper
// из периодного сна; THREAD_TERMINATED доказывает выход EL0-процесса.
fn teardown_rootkeeper(chan_id: HandleId, launch: RootkeeperLaunch) {
    handle_close(chan_id).expect("close local channel handle");
    drop(launch.channel);

    let thread_id = install_handle(Handle::new(
        KObject::Thread(launch.info.thread_object),
        Rights::WAIT | Rights::INSPECT,
    ))
    .expect("install rootkeeper thread handle");
    let observed = object_wait_one(
        thread_id,
        THREAD_TERMINATED,
        Some(5 * BOOTSTRAP_HEARTBEAT_PERIOD_NS),
    )
    .expect("wait for rootkeeper exit");
    kernel_tests::kassert!(observed & THREAD_TERMINATED != 0);
}

#[kernel_test]
fn userland_rootkeeper_bootstrap_handshake() {
    let (chan_id, launch) = spawn_rootkeeper_with_hello();
    teardown_rootkeeper(chan_id, launch);
}

#[kernel_test]
fn userland_rootkeeper_heartbeat() {
    let (chan_id, launch) = spawn_rootkeeper_with_hello();

    for expected_seq in 0u64..2 {
        let observed = object_wait_one(
            chan_id,
            CHANNEL_READABLE | CHANNEL_PEER_CLOSED,
            Some(5 * BOOTSTRAP_HEARTBEAT_PERIOD_NS),
        )
        .expect("wait for rootkeeper heartbeat");
        kernel_tests::kassert!(observed & CHANNEL_READABLE != 0);

        let message = channel_read(chan_id).expect("channel must yield heartbeat frame");
        let heartbeat = parse_bootstrap_heartbeat(message.bytes()).expect("RKBEAT must parse");
        kernel_tests::kassert_eq!(heartbeat.seq, expected_seq);
    }

    teardown_rootkeeper(chan_id, launch);
}
