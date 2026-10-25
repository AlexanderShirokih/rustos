use core::{
    future::{Future, IntoFuture},
    pin::pin,
    task::{Context, Poll, Waker},
};

/// Исполняет future хостовой модели до первого ожидания. Модель, которой нужно внешнее
/// пробуждение, проверяется отдельным опросом вместо этого вызова.
pub fn complete<F: IntoFuture>(future: F) -> F::Output {
    match pin!(future.into_future()).poll(&mut Context::from_waker(Waker::noop())) {
        Poll::Ready(output) => output,
        Poll::Pending => panic!("a mock channel operation did not complete at once"),
    }
}
