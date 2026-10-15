use core::{
    future::Future,
    pin::Pin,
    task::{Context, Poll},
};

/// Future из [`yield_now`].
#[must_use = "future does nothing until awaited"]
pub struct YieldNow {
    yielded: bool,
}

impl Future for YieldNow {
    type Output = ();

    fn poll(mut self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<()> {
        if self.yielded {
            return Poll::Ready(());
        }
        self.yielded = true;
        cx.waker().wake_by_ref();
        Poll::Pending
    }
}

/// Ставит задачу в конец очереди готовых, пропуская вперёд задачи, разбуженные раньше.
pub const fn yield_now() -> YieldNow {
    YieldNow { yielded: false }
}
