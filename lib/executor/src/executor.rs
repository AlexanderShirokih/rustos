use alloc::{
    boxed::Box,
    collections::{BTreeMap, VecDeque},
    sync::Arc,
    task::Wake,
};
use core::{
    cell::{Cell, RefCell},
    future::Future,
    pin::{Pin, pin},
    sync::atomic::{AtomicBool, Ordering::SeqCst},
    task::{Context, Poll, Waker},
};

use kernel_objects::Signal;
use spin::Mutex;
use syscall::Timeout;

use crate::reactor::Reactor;

type TaskId = u64;

const MAIN_TASK: TaskId = 0;

/// Однопоточный executor задач и reactor ожидания объектов ядра.
///
/// Задачи выполняются на потоке, который зовёт [`Self::block_on`], и делят его IPC-буфер.
/// Задача выполняет только неблокирующие вызовы ядра. Блокирующий вызов останавливает все
/// задачи потока. Задача ждёт сигналы объекта ядра через [`Reactor::wait`].
pub struct Executor {
    shared: Arc<Shared>,
    tasks: RefCell<BTreeMap<TaskId, Task>>,
    next_id: Cell<TaskId>,
    reactor: Reactor,
}

pub(crate) struct Shared {
    ready: Mutex<VecDeque<TaskId>>,
    pub(crate) notify: Signal,
    pub(crate) sleeping: AtomicBool,
}

struct Task {
    future: Pin<Box<dyn Future<Output = ()>>>,
    waker: Arc<TaskWaker>,
}

struct TaskWaker {
    id: TaskId,
    queued: AtomicBool,
    shared: Arc<Shared>,
}

impl Executor {
    /// Создаёт executor и сигнал его пробуждения.
    pub fn new() -> kernel_objects::Result<Self> {
        Ok(Self {
            shared: Arc::new(Shared {
                ready: Mutex::new(VecDeque::new()),
                notify: Signal::create()?,
                sleeping: AtomicBool::new(false),
            }),
            tasks: RefCell::new(BTreeMap::new()),
            next_id: Cell::new(MAIN_TASK.saturating_add(1)),
            reactor: Reactor::new(),
        })
    }

    /// Порождает задачу. Задача выполняется внутри [`Self::block_on`]. Задачи, не завершённые
    /// к возврату главной, освобождаются вместе с executor.
    pub fn spawn(&self, task: impl Future<Output = ()> + 'static) {
        let id = self.next_id.get();
        self.next_id.set(id.saturating_add(1));

        let waker = self.waker(id);
        self.tasks.borrow_mut().insert(
            id,
            Task {
                future: Box::pin(task),
                waker: Arc::clone(&waker),
            },
        );
        waker.wake_by_ref();
    }

    /// Reactor, через который задачи этого executor ждут объекты ядра.
    #[must_use]
    pub fn reactor(&self) -> Reactor {
        self.reactor.clone()
    }

    /// Выполняет `main` и порождённые задачи до готовности `main` и возвращает её результат.
    /// Пока ни одна задача не разбужена, поток ждёт в ядре.
    pub fn block_on<F: Future>(&self, main: F) -> F::Output {
        let mut main = pin!(main);
        let main_task = self.waker(MAIN_TASK);
        let main_waker = Waker::from(Arc::clone(&main_task));
        main_task.wake_by_ref();

        loop {
            // Проход опрашивает только записи, стоявшие в очереди при его начале. Записи,
            // добавленные во время прохода, ждут следующего, и самопробуждение задачи не
            // держит поток в одном проходе.
            let batch = self.shared.ready.lock().len();
            for _ in 0..batch {
                let Some(id) = self.shared.ready.lock().pop_front() else {
                    break;
                };
                if id != MAIN_TASK {
                    self.poll_task(id);
                    continue;
                }
                main_task.queued.store(false, SeqCst);
                if let Poll::Ready(output) =
                    main.as_mut().poll(&mut Context::from_waker(&main_waker))
                {
                    return output;
                }
            }

            // `sleeping` выставлен до повторной проверки очереди, а пробуждение читает его после
            // записи в очередь, поэтому пробуждение между проверкой и ожиданием поднимает
            // `notify`. Непустая очередь не ждёт в ядре, но ход reactor выдаёт готовность
            // объектов задачам, которые их ждут, пока другие задачи уступают поток.
            self.shared.sleeping.store(true, SeqCst);
            let wait = if self.shared.ready.lock().is_empty() {
                Timeout::INFINITE
            } else {
                self.shared.sleeping.store(false, SeqCst);
                Timeout::POLL
            };
            self.reactor.turn(&self.shared, wait);
            self.shared.sleeping.store(false, SeqCst);
        }
    }

    fn waker(&self, id: TaskId) -> Arc<TaskWaker> {
        Arc::new(TaskWaker {
            id,
            queued: AtomicBool::new(false),
            shared: Arc::clone(&self.shared),
        })
    }

    fn poll_task(&self, id: TaskId) {
        // Задача опрашивается вне заёма таблицы задач, и `spawn` из задачи добавляет в неё
        // новые.
        let Some(mut task) = self.tasks.borrow_mut().remove(&id) else {
            return;
        };
        task.waker.queued.store(false, SeqCst);
        let waker = Waker::from(Arc::clone(&task.waker));
        if task
            .future
            .as_mut()
            .poll(&mut Context::from_waker(&waker))
            .is_pending()
        {
            self.tasks.borrow_mut().insert(id, task);
        }
    }
}

impl Wake for TaskWaker {
    fn wake(self: Arc<Self>) {
        self.wake_by_ref();
    }

    fn wake_by_ref(self: &Arc<Self>) {
        if self.queued.swap(true, SeqCst) {
            return;
        }
        self.shared.ready.lock().push_back(self.id);
        if self.shared.sleeping.load(SeqCst) {
            // `notify` принадлежит `Shared` и открыт, пока жив этот `Waker`.
            let _ = self.shared.notify.notify_all();
        }
    }
}
