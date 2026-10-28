//! Проверки выполнения и пробуждения задач [`Executor`] из EL0.

use alloc::{rc::Rc, sync::Arc, vec::Vec};
use core::{
    cell::{Cell, RefCell},
    future::poll_fn,
    num::NonZeroU64,
    sync::atomic::{AtomicBool, AtomicI64, AtomicU64, Ordering::SeqCst},
    task::{Poll, Waker},
    time::Duration,
};

use component_startup::{SessionEvent, SessionEvents};
use executor::{Executor, yield_now};
use kernel_objects::{Error, Port, Signal};
use runtime::{Instant, Mutex, Timeout, port_send, sleep, spawn_owned};
use syscall::{Rights, SIGNALED, SyscallError, WAIT_MANY_MAX_COUNT, WaitItem};

use crate::{fixture::JOIN_TIMEOUT, ipc_raw};

const SEND_TIMEOUT: Timeout = Timeout::from_secs(3);
const ROUND_DELAY: Duration = Duration::from_millis(50);
const SLEEP_DEADLINE: Timeout = Timeout::from_millis(300);

const HANDOFF_VALUE: u64 = 0x0e7e_c000;
const FIRST_BADGE: NonZeroU64 = NonZeroU64::new(0xe7ec_0001).expect("badge is non-zero");
const SECOND_BADGE: NonZeroU64 = NonZeroU64::new(0xe7ec_0002).expect("badge is non-zero");

struct Handoff {
    value: AtomicU64,
    waker: Mutex<Option<Waker>>,
    polled: Signal,
    woken: AtomicBool,
}

pub(crate) fn executor() -> Executor {
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

async fn first_frame(port: &Port) -> kernel_objects::Result<u64> {
    let mut events = SessionEvents::new(port);
    loop {
        match events.recv().await? {
            Some(SessionEvent::Frame(frame)) => return Ok(frame.resources.badge),
            Some(SessionEvent::Gone(_)) => {}
            None => return Err(Error::Syscall(SyscallError::BadHandle)),
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

static FIRST_SEND: AtomicI64 = AtomicI64::new(i64::MIN);
static SECOND_SEND: AtomicI64 = AtomicI64::new(i64::MIN);

#[test_harness::test]
fn executor_serves_two_ports_on_one_thread() {
    let first = Port::create().expect("port create");
    let second = Port::create().expect("port create");
    let first_sender = send_frame(session(&first, FIRST_BADGE), &FIRST_SEND);
    let second_sender = send_frame(session(&second, SECOND_BADGE), &SECOND_SEND);

    let executor = executor();
    let spawned = Rc::new(Cell::new(None));
    let spawned_badge = Rc::clone(&spawned);
    executor.spawn(async move {
        spawned_badge.set(Some(first_frame(&second).await));
    });
    let main = executor.block_on(async {
        let badge = first_frame(&first).await;
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
fn executors_on_two_threads_resume_their_own_tasks() {
    let first = Port::create().expect("port create");
    let second = Port::create().expect("port create");
    let first_sender = send_frame(session(&first, FIRST_BADGE), &FIRST_SEND);
    let second_sender = send_frame(session(&second, SECOND_BADGE), &SECOND_SEND);

    let worker = spawn_owned(move || {
        let badge = executor().block_on(first_frame(&second));

        u32::from(badge == Ok(SECOND_BADGE.get()))
    })
    .expect("spawn worker");
    let main = executor().block_on(first_frame(&first));

    worker.wait(JOIN_TIMEOUT).expect("worker exits");
    test_harness::check_eq!(worker.exit_code(), Ok(1));
    worker.join(JOIN_TIMEOUT).expect("worker joins");
    test_harness::check_eq!(main, Ok(FIRST_BADGE.get()));
    first_sender.join(JOIN_TIMEOUT).expect("sender joins");
    second_sender.join(JOIN_TIMEOUT).expect("sender joins");
}

#[test_harness::test]
fn a_nested_executor_returns_the_wait_context_of_the_outer_one() {
    let port = Port::create().expect("port create");

    let (slept, badge) = executor().block_on(async {
        let start = Instant::now();
        executor().block_on(executor::sleep(SLEEP_DEADLINE));
        let slept = Instant::now().since(start);
        let sender = send_frame(session(&port, FIRST_BADGE), &FIRST_SEND);
        let badge = first_frame(&port).await;
        sender.join(JOIN_TIMEOUT).expect("sender joins");

        (slept, badge)
    });

    test_harness::check!(slept >= SLEEP_DEADLINE);
    test_harness::check_eq!(badge, Ok(FIRST_BADGE.get()));
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
    let failed = Rc::new(Cell::new(None));
    let waiter_result = Rc::clone(&failed);
    executor.spawn(async move {
        waiter_result.set(Some(executor::wait(item).await));
    });
    executor.spawn(async move {
        // Ожидатель регистрирует запись до закрытия.
        yield_now().await;
        closing.borrow_mut().take();
    });
    let main = executor.block_on(async {
        let badge = first_frame(&port).await;
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
    for signal in signals.iter() {
        let item = WaitItem::new(signal.as_raw(), SIGNALED);
        let completed = Rc::clone(&completed);
        executor.spawn(async move {
            if executor::wait(item).await.is_ok() {
                completed.set(completed.get().saturating_add(1));
            }
        });
    }
    let extra = Signal::create().expect("signal create");

    let (refused, waiting) = executor.block_on(async {
        // Порождённые задачи регистрируют ожидания до следующего опроса главной.
        yield_now().await;
        let refused = executor::wait(WaitItem::new(extra.as_raw(), SIGNALED)).await;
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
    for (index, signal) in ready.iter().enumerate() {
        let item = WaitItem::new(signal.as_raw(), SIGNALED);
        let resumed = Rc::clone(&resumed);
        executor.spawn(async move {
            while executor::wait(item).await.is_ok() {
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
            let stopped = executor::wait(WaitItem::new(stop.as_raw(), SIGNALED)).await;
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
fn executor_resumes_a_sleep_while_another_task_receives() {
    let port = Port::create().expect("port create");

    let executor = executor();
    let received = Rc::new(Cell::new(false));
    let slept = Rc::new(Cell::new(None));
    let sleeper = (Rc::clone(&received), Rc::clone(&slept));
    let start = Instant::now();
    executor.spawn(async move {
        let (received, slept) = sleeper;
        executor::sleep(SLEEP_DEADLINE).await;
        slept.set(Some((Instant::now().since(start), received.get())));
    });

    let sender = send_frame(session(&port, FIRST_BADGE), &FIRST_SEND);
    let main = executor.block_on(async {
        let badge = first_frame(&port).await;
        received.set(true);
        until(|| slept.get().is_some()).await;
        badge
    });

    test_harness::check_eq!(main, Ok(FIRST_BADGE.get()));

    let (elapsed, received_first) = slept.get().expect("sleeper resumed");
    test_harness::check!(received_first);
    test_harness::check!(elapsed >= SLEEP_DEADLINE);
    sender.join(JOIN_TIMEOUT).expect("sender joins");
    test_harness::check_eq!(FIRST_SEND.load(SeqCst), 0);
}

#[test_harness::test]
fn executor_timeout_drops_a_wait_without_a_signal() {
    struct Dropped(Rc<Cell<bool>>);

    impl Drop for Dropped {
        fn drop(&mut self) {
            self.0.set(true);
        }
    }

    let signal = Signal::create().expect("signal create");
    let item = WaitItem::new(signal.as_raw(), SIGNALED);

    let executor = executor();
    let dropped = Rc::new(Cell::new(false));
    let guard = Dropped(Rc::clone(&dropped));

    let waited = executor.block_on(executor::timeout(SLEEP_DEADLINE, {
        async move {
            let _guard = guard;
            executor::wait(item).await
        }
    }));

    test_harness::check!(waited.is_none());
    test_harness::check!(dropped.get());
}

#[test_harness::test]
fn executor_cancels_an_owned_task_while_it_is_being_polled() {
    let executor = executor();
    let holder = Rc::new(RefCell::new(None::<executor::TaskHandle>));
    let owned = holder.clone();
    let marker = Rc::new(());
    let held = marker.clone();
    let task = executor
        .spawn_now(async move {
            yield_now().await;
            drop(owned.borrow_mut().take());
            core::future::pending::<()>().await;
            drop(held);
        })
        .abort_on_drop();
    *holder.borrow_mut() = Some(task);
    let completed = executor.block_on(executor::timeout(
        SEND_TIMEOUT,
        until(|| Rc::strong_count(&marker) == 1),
    ));
    test_harness::check!(completed.is_some());
    test_harness::check_eq!(Rc::strong_count(&marker), 1);
    test_harness::check!(holder.borrow().is_none());
}

#[test_harness::test]
fn executor_mutex_waits_without_stopping_tasks_and_cancels_waiters() {
    let executor = executor();
    let mutex = Rc::new(executor::Mutex::new(0));
    let entered = Rc::new(Cell::new(false));
    let cancelled_entered = Rc::new(Cell::new(false));
    let unrelated = Rc::new(Cell::new(false));

    executor.block_on(async {
        let mut guard = mutex.lock().await;

        let waiting = mutex.clone();
        let cancelled = cancelled_entered.clone();
        let abandoned = executor
            .spawn_now(async move {
                let _guard = waiting.lock().await;
                cancelled.set(true);
            })
            .abort_on_drop();

        let waiting = mutex.clone();
        let completed = entered.clone();
        let survivor = executor
            .spawn_now(async move {
                let mut guard = waiting.lock().await;
                test_harness::check_eq!(*guard, 7);
                *guard = 8;
                completed.set(true);
            })
            .abort_on_drop();

        drop(abandoned);
        let other = unrelated.clone();
        executor.spawn(async move { other.set(true) });

        until(|| unrelated.get()).await;
        test_harness::check!(!entered.get());
        *guard = 7;
        drop(guard);

        let completed = executor::timeout(SEND_TIMEOUT, until(|| entered.get())).await;
        test_harness::check!(completed.is_some());
        test_harness::check_eq!(*mutex.lock().await, 8);
        test_harness::check!(!cancelled_entered.get());
        test_harness::check!(survivor.is_finished());
    });
}

#[test_harness::test]
fn executor_mutex_releases_the_value_when_its_owner_is_cancelled() {
    let executor = executor();
    let mutex = Rc::new(executor::Mutex::new(0));
    let protected = mutex.clone();
    let owner = executor
        .spawn_now(async move {
            let mut guard = protected.lock().await;
            *guard = 7;
            core::future::pending::<()>().await;
            drop(guard);
        })
        .abort_on_drop();

    let waiting = mutex.clone();
    let entered = Rc::new(Cell::new(false));
    let completed = entered.clone();
    let survivor = executor
        .spawn_now(async move {
            let guard = waiting.lock().await;
            test_harness::check_eq!(*guard, 7);
            completed.set(true);
        })
        .abort_on_drop();

    drop(owner);

    let completed = executor.block_on(executor::timeout(SEND_TIMEOUT, until(|| entered.get())));
    test_harness::check!(completed.is_some());
    test_harness::check!(survivor.is_finished());
}

#[test_harness::test]
fn executor_wait_any_returns_the_index_and_releases_cancelled_registrations() {
    let executor = executor();
    let first = Signal::create().expect("first signal");
    let second = Signal::create().expect("second signal");
    let items = [
        WaitItem::new(first.as_raw(), SIGNALED),
        WaitItem::new(second.as_raw(), SIGNALED),
    ];

    executor.block_on(async {
        for _ in 0..WAIT_MANY_MAX_COUNT {
            test_harness::check!(
                executor::timeout(Timeout::POLL, executor::wait_any(&items))
                    .await
                    .is_none()
            );
        }

        second.notify_all().expect("second signal raised");
        test_harness::check_eq!(executor::wait_any(&items).await, Ok(1));

        for _ in 0..WAIT_MANY_MAX_COUNT {
            test_harness::check!(
                executor::timeout(Timeout::POLL, executor::wait_any(&items))
                    .await
                    .is_none()
            );
        }

        test_harness::check_eq!(executor::wait_any(&items).await, Ok(1));
    });
}

#[test_harness::test]
fn executor_keeps_operation_resources_after_its_result_waiter_is_dropped() {
    let executor = executor();
    let result = executor.spawn(async { 7 });
    test_harness::check_eq!(executor.block_on(result), Some(7));

    let marker = Rc::new(());
    let held = marker.clone();
    let entered = Rc::new(Cell::new(false));
    let started = entered.clone();
    let released = Rc::new(Cell::new(false));
    let release = released.clone();
    let finished = Rc::new(Cell::new(false));
    let completed = finished.clone();
    let result = executor.spawn(async move {
        started.set(true);
        until(|| release.get()).await;
        drop(held);
        completed.set(true);
    });

    drop(result);
    let completed = executor.block_on(executor::timeout(SEND_TIMEOUT, async {
        until(|| entered.get()).await;
        test_harness::check_eq!(Rc::strong_count(&marker), 2);

        released.set(true);
        until(|| finished.get()).await;
    }));
    test_harness::check!(completed.is_some());
    test_harness::check_eq!(Rc::strong_count(&marker), 1);

    let cancelled = {
        let executor = crate::executor::executor();
        executor.spawn(core::future::pending::<()>())
    };

    let mut cancelled = core::pin::pin!(cancelled);
    test_harness::check_eq!(
        cancelled
            .as_mut()
            .poll(&mut core::task::Context::from_waker(Waker::noop())),
        Poll::Ready(None)
    );
}
