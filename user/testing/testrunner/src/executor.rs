//! Проверки выполнения и пробуждения задач [`Executor`] из EL0.

use alloc::{rc::Rc, sync::Arc, vec::Vec};
use core::{
    cell::{Cell, RefCell},
    future::{Future, poll_fn},
    num::NonZeroU64,
    pin::pin,
    sync::atomic::{AtomicBool, AtomicI64, AtomicU64, Ordering::SeqCst},
    task::{Poll, Waker},
    time::Duration,
};

use component_startup::{Polled, SessionEvent, SessionEvents};
use executor::{Channel, Executor, Reactor, channel, yield_now};
use kernel_objects::{Error, Port, Signal};
use runtime::{Mutex, Timeout, port_send, sleep, spawn_owned};
use syscall::{READABLE, Rights, SIGNALED, SyscallError, WAIT_MANY_MAX_COUNT, WaitItem};

use crate::{fixture::JOIN_TIMEOUT, ipc_raw};

const SEND_TIMEOUT: Timeout = Timeout::from_secs(3);
const ROUND_DELAY: Duration = Duration::from_millis(50);

const HANDOFF_VALUE: u64 = 0x0e7e_c000;
const FIRST_BADGE: NonZeroU64 = NonZeroU64::new(0xe7ec_0001).expect("badge is non-zero");
const SECOND_BADGE: NonZeroU64 = NonZeroU64::new(0xe7ec_0002).expect("badge is non-zero");
const CHANNEL_VALUES: [u64; 3] = [0x0c4a_0001, 0x0c4a_0002, 0x0c4a_0003];

struct Handoff {
    value: AtomicU64,
    waker: Mutex<Option<Waker>>,
    polled: Signal,
    woken: AtomicBool,
}

fn executor() -> Executor {
    Executor::new().expect("executor create")
}

fn raised_signal() -> Signal {
    let signal = Signal::create().expect("signal create");
    signal.notify_all().expect("signal raise");
    signal
}

fn session(port: &Port, badge: NonZeroU64) -> Port {
    port.duplicate(Rights::WRITE | Rights::TRANSFER, Some(badge))
        .expect("mint session capability")
}

fn send_frame(session: Port, result: &'static AtomicI64) -> runtime::JoinHandle {
    result.store(i64::MIN, SeqCst);
    spawn_owned(move || {
        ipc_raw::write_frame(&[], &[]);
        result.store(port_send(session.as_raw(), SEND_TIMEOUT), SeqCst);
        0
    })
    .expect("spawn sender")
}

async fn first_frame(reactor: &Reactor, port: &Port) -> kernel_objects::Result<u64> {
    let mut events = SessionEvents::new(port);
    loop {
        match events.try_recv() {
            Polled::Event(SessionEvent::Frame(frame)) => return Ok(frame.resources.badge),
            Polled::Event(SessionEvent::Gone(_)) => {}
            Polled::Empty => reactor.wait(WaitItem::new(port.as_raw(), READABLE)).await?,
            Polled::Closed => return Err(Error::Syscall(SyscallError::BadHandle)),
        }
    }
}

async fn until(done: impl Fn() -> bool) {
    while !done() {
        yield_now().await;
    }
}

#[test_harness::test]
fn executor_wakes_a_task_from_another_thread() {
    let handoff = Arc::new(Handoff {
        value: AtomicU64::new(0),
        waker: Mutex::new(None),
        polled: Signal::create().expect("signal create"),
        woken: AtomicBool::new(false),
    });
    let remote = Arc::clone(&handoff);
    let setter = spawn_owned(move || {
        // Значение появляется после того, как главная задача вернула `Pending` и в очереди
        // готовых не осталось задач, поэтому executor продолжает её только через `Waker::wake`.
        remote
            .polled
            .wait(SIGNALED, SEND_TIMEOUT)
            .expect("main task polled");
        let waker = remote.waker.lock().take();
        remote.value.store(HANDOFF_VALUE, SeqCst);
        if let Some(waker) = waker {
            remote.woken.store(true, SeqCst);
            waker.wake();
        }
        0
    })
    .expect("spawn setter");

    let value = executor().block_on(poll_fn(|cx| {
        *handoff.waker.lock() = Some(cx.waker().clone());
        match handoff.value.load(SeqCst) {
            0 => {
                handoff.polled.notify_all().expect("signal raise");
                Poll::Pending
            }
            value => Poll::Ready(value),
        }
    }));

    test_harness::check_eq!(value, HANDOFF_VALUE);
    test_harness::check!(handoff.woken.load(SeqCst));
    setter.join(JOIN_TIMEOUT).expect("setter joins");
}

#[test_harness::test]
fn executor_runs_spawned_tasks() {
    let executor = executor();
    let flags = Rc::new([Cell::new(false), Cell::new(false)]);
    for index in 0..2 {
        let flags = Rc::clone(&flags);
        executor.spawn(async move {
            if let Some(flag) = flags.get(index) {
                flag.set(true);
            }
        });
    }

    executor.block_on(until(|| flags.iter().all(Cell::get)));
    test_harness::check!(flags.iter().all(Cell::get));
}

static FIRST_SEND: AtomicI64 = AtomicI64::new(i64::MIN);
static SECOND_SEND: AtomicI64 = AtomicI64::new(i64::MIN);

#[test_harness::test]
fn executor_serves_two_ports_on_one_thread() {
    let first = Port::create().expect("port create");
    let second = Port::create().expect("port create");
    let first_sender = send_frame(session(&first, FIRST_BADGE), &FIRST_SEND);
    let second_sender = send_frame(session(&second, SECOND_BADGE), &SECOND_SEND);

    let executor = executor();
    let reactor = executor.reactor();
    let spawned = Rc::new(Cell::new(None));
    let spawned_badge = Rc::clone(&spawned);
    let task_reactor = reactor.clone();
    executor.spawn(async move {
        spawned_badge.set(Some(first_frame(&task_reactor, &second).await));
    });
    let main = executor.block_on(async {
        let badge = first_frame(&reactor, &first).await;
        until(|| spawned.get().is_some()).await;
        badge
    });

    test_harness::check_eq!(main, Ok(FIRST_BADGE.get()));
    test_harness::check_eq!(spawned.get(), Some(Ok(SECOND_BADGE.get())));
    first_sender.join(JOIN_TIMEOUT).expect("sender joins");
    second_sender.join(JOIN_TIMEOUT).expect("sender joins");
    test_harness::check_eq!(FIRST_SEND.load(SeqCst), 0);
    test_harness::check_eq!(SECOND_SEND.load(SeqCst), 0);
}

#[test_harness::test]
fn executor_fails_a_wait_on_a_closed_handle() {
    let port = Port::create().expect("port create");
    let sender = send_frame(session(&port, FIRST_BADGE), &FIRST_SEND);
    let closing = Rc::new(RefCell::new(Some(Signal::create().expect("signal create"))));
    let item = closing
        .borrow()
        .as_ref()
        .map(|signal| WaitItem::new(signal.as_raw(), SIGNALED))
        .expect("signal is open");

    let executor = executor();
    let reactor = executor.reactor();
    let failed = Rc::new(Cell::new(None));
    let waiter_result = Rc::clone(&failed);
    let waiter_reactor = reactor.clone();
    executor.spawn(async move {
        waiter_result.set(Some(waiter_reactor.wait(item).await));
    });
    executor.spawn(async move {
        // Ожидатель регистрирует запись до закрытия.
        yield_now().await;
        closing.borrow_mut().take();
    });
    let main = executor.block_on(async {
        let badge = first_frame(&reactor, &port).await;
        until(|| failed.get().is_some()).await;
        badge
    });

    test_harness::check!(matches!(failed.get(), Some(Err(_))));
    test_harness::check_eq!(main, Ok(FIRST_BADGE.get()));
    sender.join(JOIN_TIMEOUT).expect("sender joins");
    test_harness::check_eq!(FIRST_SEND.load(SeqCst), 0);
}

#[test_harness::test]
fn executor_refuses_a_wait_over_the_limit() {
    // Последнюю запись `SignalWaitMany` занимает пробуждение executor.
    let held = WAIT_MANY_MAX_COUNT - 1;
    let signals: Rc<Vec<Signal>> = Rc::new(
        (0..held)
            .map(|_| Signal::create().expect("signal create"))
            .collect(),
    );
    let completed = Rc::new(Cell::new(0usize));

    let executor = executor();
    let reactor = executor.reactor();
    for signal in signals.iter() {
        let item = WaitItem::new(signal.as_raw(), SIGNALED);
        let completed = Rc::clone(&completed);
        let reactor = reactor.clone();
        executor.spawn(async move {
            if reactor.wait(item).await.is_ok() {
                completed.set(completed.get().saturating_add(1));
            }
        });
    }
    let extra = Signal::create().expect("signal create");

    let (refused, waiting) = executor.block_on(async {
        // Порождённые задачи регистрируют ожидания до следующего опроса главной.
        yield_now().await;
        let refused = reactor.wait(WaitItem::new(extra.as_raw(), SIGNALED)).await;
        let waiting = completed.get();
        for signal in signals.iter() {
            signal.notify_all().expect("signal raise");
        }
        until(|| completed.get() == held).await;
        (refused, waiting)
    });

    test_harness::check_eq!(
        refused,
        Err(Error::Syscall(SyscallError::ResourceExhausted))
    );
    test_harness::check_eq!(waiting, 0);
    test_harness::check_eq!(completed.get(), held);
}

#[test_harness::test]
fn executor_alternates_constantly_ready_objects() {
    let ready = [raised_signal(), raised_signal()];
    let resumed = Rc::new([Cell::new(0u64), Cell::new(0u64)]);

    let executor = executor();
    let reactor = executor.reactor();
    for (index, signal) in ready.iter().enumerate() {
        let item = WaitItem::new(signal.as_raw(), SIGNALED);
        let resumed = Rc::clone(&resumed);
        let reactor = reactor.clone();
        executor.spawn(async move {
            while reactor.wait(item).await.is_ok() {
                if let Some(count) = resumed.get(index) {
                    count.set(count.get().saturating_add(1));
                }
            }
        });
    }

    let rounds = executor.block_on(async {
        let mut rounds = [[0u64; 2]; 2];
        for round in &mut rounds {
            let stop = Arc::new(Signal::create().expect("signal create"));
            let remote_stop = Arc::clone(&stop);
            let timer = spawn_owned(move || {
                sleep(ROUND_DELAY);
                remote_stop.notify_all().expect("stop raise");
                0
            })
            .expect("spawn timer");
            let stopped = reactor.wait(WaitItem::new(stop.as_raw(), SIGNALED)).await;
            test_harness::check!(stopped.is_ok());
            timer.join(JOIN_TIMEOUT).expect("timer joins");
            *round = [
                resumed.first().map_or(0, Cell::get),
                resumed.get(1).map_or(0, Cell::get),
            ];
        }
        rounds
    });

    let [first, second] = rounds;
    test_harness::check!(first.iter().all(|count| *count > 0));
    test_harness::check!(
        second
            .iter()
            .zip(first)
            .all(|(later, earlier)| *later > earlier)
    );
}

#[test_harness::test]
fn executor_channel_passes_values_from_a_thread() {
    let Channel {
        sender,
        mut receiver,
    } = channel();
    let polled = Arc::new(Signal::create().expect("signal create"));
    let remote = Arc::clone(&polled);
    let thread = spawn_owned(move || {
        // Значения уходят после того, как задача вернула `Pending` на пустом канале, поэтому
        // приём возобновляет отправка.
        remote
            .wait(SIGNALED, SEND_TIMEOUT)
            .expect("receiver polled");
        for value in CHANNEL_VALUES {
            sender.send(value).expect("receiver is alive");
        }
        drop(sender);
        0
    })
    .expect("spawn sender");

    let values = executor().block_on(async {
        let first = {
            let mut first = pin!(receiver.recv());
            poll_fn(|cx| {
                let polled_first = first.as_mut().poll(cx);
                if polled_first.is_pending() {
                    polled.notify_all().expect("signal raise");
                }
                polled_first
            })
            .await
        };
        let mut values: Vec<u64> = first.into_iter().collect();
        while let Some(value) = receiver.recv().await {
            values.push(value);
        }
        values
    });

    test_harness::check_eq!(values.as_slice(), CHANNEL_VALUES.as_slice());
    thread.join(JOIN_TIMEOUT).expect("sender joins");
}

static RETURNED: AtomicU64 = AtomicU64::new(0);

#[test_harness::test]
fn executor_channel_returns_a_value_to_the_sender() {
    let Channel { sender, receiver } = channel();
    executor().block_on(async move { drop(receiver) });

    RETURNED.store(0, SeqCst);
    let thread = spawn_owned(move || {
        RETURNED.store(sender.send(HANDOFF_VALUE).err().unwrap_or(0), SeqCst);
        0
    })
    .expect("spawn sender");
    thread.join(JOIN_TIMEOUT).expect("sender joins");

    test_harness::check_eq!(RETURNED.load(SeqCst), HANDOFF_VALUE);
}
