//! bootstrap-log: дренаж кадров (log, unknown) и выход по
//! PEER_CLOSED - чисто сигнально, без user-процесса и таймера.

use bootstrap::{BootstrapClient, LOG_MESSAGE_MAX};
use ipc::wire::Str;
use kernel_tests::kernel_test;
use kobject::{
    Channel, EVENT_SIGNALED, Event, Handle, KObject, Message, Rights, handle_close, install_handle,
    object_wait_one,
};
use scheduler::{Priority, SchedulerServiceExt, SpawnConfig};

use crate::bootstrap::{ChannelTransport, run_bootstrap_log};

#[kernel_test]
fn bootstrap_log_exits_on_peer_close() {
    let (local, peer) = Channel::create_pair(0);

    let event = Event::new();
    let event_id = install_handle(Handle::new(
        KObject::Event(event.clone()),
        Rights::WAIT | Rights::INSPECT,
    ))
    .expect("install_handle must succeed");

    let scheduler = super::scheduler().clone();
    let event_for_task = event.clone();
    scheduler
        .spawn(
            SpawnConfig::new("bootstrap-log-test").priority(Priority::normal()),
            move || {
                run_bootstrap_log(&local);
                event_for_task.signal(EVENT_SIGNALED, 0);
            },
        )
        .expect("bootstrap-log task spawn must succeed");

    let peer_id = install_handle(Handle::new(KObject::Channel(peer.clone()), Rights::WRITE))
        .expect("install peer channel handle");
    let client = BootstrapClient::new(ChannelTransport::new(peer_id));
    client
        .log(Str::<LOG_MESSAGE_MAX>::new("boot ok").expect("log message must fit"))
        .expect("log write must succeed");

    peer.write(Message::from_bytes(b"garbage").expect("garbage fits inline"))
        .expect("garbage write must succeed");

    handle_close(peer_id).expect("close peer channel handle");
    drop(peer);

    let observed = object_wait_one(event_id, EVENT_SIGNALED, Some(5_000_000_000))
        .expect("bootstrap-log must exit after peer close");
    kernel_tests::kassert!(observed & EVENT_SIGNALED != 0);
}
