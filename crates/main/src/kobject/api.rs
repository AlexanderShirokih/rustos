//! Имитирует будущий syscall-слой: на вход - `HandleId`, на выход -
//! `Result<_, IpcError>`. Внутри идёт через [`runtime()`](super::runtime)
//! к per-process `HandleTable` и scheduler-у. Никаких `Arc<dyn
//! KernelObject>` наружу не утекает.
//!
//! Сейчас реализована только операция [`object_wait_one`] - её хватит
//! для pilot-демо timer-сервиса. Остальные функции (`channel_*`,
//! `handle_close`, ...) добавятся по мере миграции конкретных сервисов
//! и помечены `// TODO(kobject-migration)`.

use alloc::sync::Arc;

use collections::LockCell;

use super::{
    errors::IpcError,
    handle::{Handle, HandleId},
    rights::Rights,
    runtime::{ParkState, runtime},
    wait::ParkWaker,
};

/// Устанавливает [`Handle`] в handle-table текущего процесса и
/// возвращает свежий [`HandleId`]. Удобный сахар для перехода между
/// "у меня есть `Arc<KO>`" и handle-based API.
pub fn install_handle(handle: Handle) -> Result<HandleId, IpcError> {
    let table = runtime()
        .current_handle_table()
        .ok_or(IpcError::BadHandle)?;
    table.with_lock(|tbl| tbl.insert(handle))
}

/// Атомарно меняет биты сигнального состояния KO (поднимает `set`,
/// снимает `clear`). Требует [`Rights::SIGNAL`] на handle. Без
/// signal_state у KO - `WrongType`.
pub fn object_signal(handle_id: HandleId, set: u32, clear: u32) -> Result<(), IpcError> {
    let runtime = runtime();
    let table = runtime.current_handle_table().ok_or(IpcError::BadHandle)?;
    let object = table.with_lock(|tbl| tbl.clone_object(handle_id, Rights::SIGNAL))?;
    let signal_state = object.signal_state().ok_or(IpcError::WrongType)?;
    signal_state.signal(set, clear);
    Ok(())
}

/// Ждёт пока на KO, к которому относится `handle_id`, не поднимется
/// хотя бы один бит из `signals`. Возвращает наблюдённую маску
/// сигналов. На `timeout_ns = Some(0)` - полу-non-blocking poll.
///
/// - `BadHandle` / `AccessDenied`: проблемы с handle (право `WAIT`).
/// - `WrongType`: KO не сигнализуем (например, Process в текущей фазе).
/// - `Timeout`: истёк дедлайн до сигнала.
pub fn object_wait_one(
    handle_id: HandleId,
    signals: u32,
    timeout_ns: Option<u64>,
) -> Result<u32, IpcError> {
    let runtime = runtime();
    let table = runtime.current_handle_table().ok_or(IpcError::BadHandle)?;

    // Берём Arc на KO под локом таблицы и сразу отпускаем лок.
    let object = table.with_lock(|tbl| tbl.clone_object(handle_id, Rights::WAIT))?;

    let signal_state = object.signal_state().ok_or(IpcError::WrongType)?;

    // Fast-path: уже сигналит.
    let already = signal_state.peek() & signals;
    if already != 0 {
        return Ok(already);
    }

    // Non-blocking poll: не регистрируем waiter и не паркуем поток -
    // иначе block_current_until уйдёт в context switch на дедлайне now+0.
    if timeout_ns == Some(0) {
        return Err(IpcError::Timeout);
    }

    let thread_id = runtime.current_thread_id();
    let waker_arc = Arc::new(ParkWaker::new(runtime.clone(), thread_id));
    let waker_dyn: Arc<dyn super::wait::Waker> = waker_arc.clone();

    signal_state.register_waiter(signals, waker_dyn.clone());

    // register_waiter под своим локом мог уже вызвать wake() (если
    // signal-биты были выставлены): тогда state == SIGNALED - и
    // block_current_until сразу вернётся, не паркуя поток.
    runtime.block_current_until(waker_arc.state(), timeout_ns);

    // Поток возобновился: либо по signal-стороне (state SIGNALED), либо
    // по timeout-стороне (state ещё REGISTERED - сонник снял с
    // SleepQueue). Закрываем гонку через CAS.
    match waker_arc
        .state()
        .load(core::sync::atomic::Ordering::Acquire)
    {
        ParkState::SIGNALED => Ok(waker_arc.observed() & signals),
        _ => {
            if waker_arc.claim_timeout() {
                // Снимаем waker'а из списка, чтобы поздний signal не
                // зацепил уже отпущенный поток.
                signal_state.remove_waiter(&waker_dyn);
                Err(IpcError::Timeout)
            } else {
                // Signal победил между нашей загрузкой state и CAS -
                // используем его наблюдение.
                Ok(waker_arc.observed() & signals)
            }
        }
    }
}
