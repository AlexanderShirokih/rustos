//! E2E проверка полной цепочки userland: реальный initrd-blob ->
//! `UserlandImage::parse` -> мост в `UserImage` -> запуск rootkeeper в EL0 ->
//! RKHELLO-handshake через канал.
//!
//! rootkeeper получает WRITE-конец канала как bootstrap-handle и пишет в него
//! 10 байт RKHELLO. `Channel::write` кладёт сообщение в очередь *парного*
//! конца, поэтому ядро отдаёт rootkeeper'у `peer`, а читает со `local`-конца,
//! на котором появляется `CHANNEL_READABLE`.

use alloc::vec;

use kernel_tests::kernel_test;
use kobject::{CHANNEL_READABLE, Channel, Handle, KObject, Rights};
use scheduler::{ArchContext, Priority, SchedulerServiceExt, UserProcessLaunch};
use userland_abi::{
    BOOTSTRAP_ABI_VERSION, BOOTSTRAP_HELLO_MAGIC, UserlandImage, parse_bootstrap_hello,
};
use userspace::user_image_parts_from_entry;

use crate::sched::Aarch64Context;

#[kernel_test]
fn userland_rootkeeper_bootstrap_handshake() {
    let blob = kernelspace::kernel_tests::userland_blob()
        .expect("userland blob must be present in test mode");

    let image = UserlandImage::parse(blob).expect("userland image must parse");
    let entry = image.bootstrap_entry();
    let parts = user_image_parts_from_entry(&entry, Aarch64Context::USER_VA_END)
        .expect("bridge to UserImage must succeed");
    let user_image = parts.image();

    // local остаётся у ядра для чтения; peer уходит rootkeeper'у. Запись в peer
    // кладёт сообщение в очередь local, где и поднимется CHANNEL_READABLE.
    let (local, peer) = Channel::create_pair(0);

    let peer_ko = KObject::Channel(peer);
    let boot_handle = Handle::new(peer_ko.clone(), Rights::defaults_for(&peer_ko));
    let launch = UserProcessLaunch::new()
        .initial_handles(vec![boot_handle])
        .bootstrap_handle(0);

    let info = kernelspace::kernel_tests::user_process_launcher()
        .spawn_user_process_with_launch(
            "rootkeeper-bootstrap",
            &user_image,
            Priority::highest(),
            2,
            launch,
        )
        .expect("spawn_user_process must succeed");
    kernel_tests::kassert_eq!(info.initial_handle_ids.len(), 1);

    let scheduler = kernelspace::kernel_tests::scheduler().clone();
    let mut spins = 0u64;
    while local.peek_signals() & CHANNEL_READABLE == 0 {
        scheduler.sleep_ms(10);
        spins += 1;
        kernel_tests::kassert!(spins < 500);
    }

    let message = local.read().expect("channel must yield rootkeeper hello");
    let hello = parse_bootstrap_hello(message.bytes()).expect("RKHELLO must parse");
    kernel_tests::kassert_eq!(hello.version, BOOTSTRAP_ABI_VERSION);
    kernel_tests::kassert_eq!(hello.magic, BOOTSTRAP_HELLO_MAGIC);
}
