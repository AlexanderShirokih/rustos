//! Владение capability стартовой транзакции. Приём, не сложившийся в контракт, закрывает всё,
//! что транзакция уже доставила, и не трогает ресурсы предыдущей операции программы.
//!
//! Каждая доставляемая capability выведена из порта вызывающего под своим значком
//! ([`Watched`]), и уход последнего держателя значка виден
//! отказом приёма на этом порту.

use core::{
    num::NonZeroU64,
    sync::atomic::{AtomicI64, Ordering::SeqCst},
};

use component_abi::component_startup;
use ipc::{Transport as _, wire::IpcError};
use kernel_objects::Process;
use kernel_tests::kernel_test;
use runtime::{PortTransport, Timeout};

use crate::{
    fixture_frames::{launch_entry, send_raw_frame},
    watched_capability::Watched,
};

const FIXTURE_ENTRY: &str = "/boot/startup-ownership-fixture/program";
const STALE_BUFFER_ENTRY: &str = "/boot/startup-stale-buffer-fixture/program";
const FIXTURE_NAME: &str = "startup-ownership";
const STALE_BUFFER_NAME: &str = "startup-stale-buffer";

const CALL_TIMEOUT: Timeout = Timeout::from_secs(5);

const CALL_REFUSED: i64 = 1;
const CALL_TIMED_OUT: i64 = 2;
const CALL_ANSWERED: i64 = 3;

static UNEXPECTED_CALL: AtomicI64 = AtomicI64::new(i64::MIN);

const LOG_BADGE: NonZeroU64 = NonZeroU64::new(0x71).expect("badge is non-zero");
const CONTROL_BADGE: NonZeroU64 = NonZeroU64::new(0x72).expect("badge is non-zero");
const SERVICE_BADGE: NonZeroU64 = NonZeroU64::new(0x73).expect("badge is non-zero");
const FIRST_SLOT_BADGE: NonZeroU64 = NonZeroU64::new(0x74).expect("badge is non-zero");
const SECOND_SLOT_BADGE: NonZeroU64 = NonZeroU64::new(0x75).expect("badge is non-zero");

/// Состав транзакции сверх объявления закрывает весь доставленный набор.
#[kernel_test]
fn a_set_past_the_declaration_closes_every_delivered_capability() {
    let log = Watched::create(LOG_BADGE);
    let control = Watched::create(CONTROL_BADGE);
    let service = Watched::create(SERVICE_BADGE);
    let first = Watched::create(FIRST_SLOT_BADGE);
    let second = Watched::create(SECOND_SLOT_BADGE);

    // Манифест фикстуры объявляет один слот, а транзакция содержит два.
    let _fixture = launch(|client| {
        client.start(log.deliver(), control.deliver(), service.deliver(), 2)?;
        client.slot(first.deliver())?;
        client.slot(second.deliver())
    });

    log.assert_closed();
    control.assert_closed();
    service.assert_closed();
    first.assert_closed();
    second.assert_closed();
}

/// Обрыв транзакции после базового кадра закрывает уже доставленный набор.
#[kernel_test]
fn a_transaction_torn_after_the_base_frame_closes_its_capabilities() {
    let log = Watched::create(LOG_BADGE);
    let control = Watched::create(CONTROL_BADGE);
    let service = Watched::create(SERVICE_BADGE);

    // Обещанный слот не приходит, и уход значка стартового порта обрывает приём компонента.
    let _fixture =
        launch(|client| client.start(log.deliver(), control.deliver(), service.deliver(), 1));

    log.assert_closed();
    control.assert_closed();
    service.assert_closed();
}

/// Кадр, отвергнутый приёмником, закрывает capability, которую транспорт с ним доставил.
#[kernel_test]
fn a_refused_frame_closes_the_capability_it_carried() {
    let slot = Watched::create(FIRST_SLOT_BADGE);

    // Слот до базового кадра приёмник отвергает.
    let _fixture = launch(|client| client.slot(slot.deliver()));

    slot.assert_closed();
}

/// Доставленный кадр, чей разбор отказал, закрывает все свои capability, включая не
/// названные его телом.
#[kernel_test]
fn a_corrupted_delivered_frame_closes_every_capability_it_carried() {
    let first = Watched::create(LOG_BADGE);
    let second = Watched::create(CONTROL_BADGE);
    let third = Watched::create(SERVICE_BADGE);
    let fourth = Watched::create(FIRST_SLOT_BADGE);

    // Ординал базового кадра при пустом теле отказывает разбором параметров уже после
    // переноса capability.
    let _fixture = launch_entry(FIXTURE_ENTRY, FIXTURE_NAME, |transport| {
        let carried = alloc::vec![
            first.deliver(),
            second.deliver(),
            third.deliver(),
            fourth.deliver(),
        ];
        send_raw_frame(&transport, 0, carried)
    });

    first.assert_closed();
    second.assert_closed();
    third.assert_closed();
    fourth.assert_closed();
}

/// Неожиданный вызов стартового порта reply-объекта не оставляет, и вызывающий получает
/// отказ, не дожидаясь конца процесса получателя.
#[kernel_test]
fn an_unexpected_call_refuses_its_caller() {
    UNEXPECTED_CALL.store(i64::MIN, SeqCst);

    let _fixture = launch_entry(FIXTURE_ENTRY, FIXTURE_NAME, |transport| {
        // Кадр приходит вызовом, а не штатным `cast`, и его разбор отказывает.
        send_raw_frame(&transport, 1, alloc::vec::Vec::new())?;
        let answered = transport.wait_readable(CALL_TIMEOUT);
        UNEXPECTED_CALL.store(
            match answered {
                Ok(()) => CALL_ANSWERED,
                Err(IpcError::Timeout) => CALL_TIMED_OUT,
                Err(_) => CALL_REFUSED,
            },
            SeqCst,
        );
        Ok(())
    });

    kernel_tests::kassert_eq!(UNEXPECTED_CALL.load(SeqCst), CALL_REFUSED);
}

/// Отказ первого приёма транзакции не трогает capability предыдущей операции программы.
#[kernel_test]
fn a_refused_first_receive_leaves_an_earlier_capability_alone() {
    let kept = Watched::create(FIRST_SLOT_BADGE);

    // Кадр до приёма транзакции забирает фикстура, его capability принадлежит ей, а
    // транзакция обрывается без единого своего кадра.
    let _fixture = launch_entry(STALE_BUFFER_ENTRY, STALE_BUFFER_NAME, |transport| {
        component_startup::Client::new(transport).slot(kept.deliver())
    });

    kept.assert_alive();
}

fn launch(
    send: impl FnOnce(&component_startup::Client<PortTransport>) -> Result<(), IpcError>,
) -> Process {
    launch_entry(FIXTURE_ENTRY, FIXTURE_NAME, move |transport| {
        send(&component_startup::Client::new(transport))
    })
}

struct CallInsteadOfCast<'a>(&'a PortTransport);

impl ipc::Transport for CallInsteadOfCast<'_> {
    type Resources = <PortTransport as ipc::Transport>::Resources;

    fn take_resources(&self) -> Self::Resources {
        self.0.take_resources()
    }

    fn write_message(&self, bytes: &[u8], handles: &[u32]) -> Result<(), IpcError> {
        let mut bytes = bytes.to_vec();
        let mut header = ipc::wire::Header::decode(&bytes)?;
        header.txid = 1;
        header.encode(&mut bytes)?;
        self.0.write_message(&bytes, handles)
    }

    fn read_message(
        &self,
        bytes: &mut [u8],
        handles: &mut [u32],
    ) -> Result<ipc::MessageLen, IpcError> {
        self.0.read_message(bytes, handles)
    }

    fn wait_readable(&self, timeout: Timeout) -> Result<(), IpcError> {
        self.0.wait_readable(timeout)
    }
}

/// Корректное тело start не удерживает reply до получения обещанного слота.
#[kernel_test]
fn a_valid_start_sent_as_call_does_not_wait_for_the_next_frame() {
    let log = Watched::create(LOG_BADGE);
    let control = Watched::create(CONTROL_BADGE);
    let service = Watched::create(SERVICE_BADGE);
    let outcome = core::cell::Cell::new(None);
    let _fixture = launch_entry(FIXTURE_ENTRY, FIXTURE_NAME, |transport| {
        component_startup::Client::new(CallInsteadOfCast(&transport)).start(
            log.deliver(),
            control.deliver(),
            service.deliver(),
            1,
        )?;
        outcome.set(Some(transport.wait_readable(CALL_TIMEOUT)));
        Ok(())
    });
    log.assert_closed();
    control.assert_closed();
    service.assert_closed();
    kernel_tests::kassert!(matches!(outcome.get(), Some(Err(error)) if error != IpcError::Timeout));
}
