//! bootstrap-log: дренаж кадров (hello, unknown) и выход по
//! PEER_CLOSED - чисто сигнально, без user-процесса и таймера.

use kernel_tests::kernel_test;
use kobject::{
    Channel, EVENT_SIGNALED, Event, Handle, KObject, Message, Rights, install_handle,
    object_wait_one,
};
use scheduler::{Priority, SchedulerServiceExt, SpawnConfig};
use userland_abi::{BOOTSTRAP_ABI_VERSION, BOOTSTRAP_HELLO_MAGIC, BOOTSTRAP_HELLO_SIZE};

use crate::bootstrap::run_bootstrap_log;

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

    let mut hello = [0u8; BOOTSTRAP_HELLO_SIZE];
    hello[0..8].copy_from_slice(&BOOTSTRAP_HELLO_MAGIC);
    hello[8..10].copy_from_slice(&BOOTSTRAP_ABI_VERSION.to_le_bytes());
    peer.write(Message::from_bytes(&hello).expect("hello fits inline"))
        .expect("hello write must succeed");

    peer.write(Message::from_bytes(b"garbage").expect("garbage fits inline"))
        .expect("garbage write must succeed");

    drop(peer);

    let observed = object_wait_one(event_id, EVENT_SIGNALED, Some(5_000_000_000))
        .expect("bootstrap-log must exit after peer close");
    kernel_tests::kassert!(observed & EVENT_SIGNALED != 0);
}
