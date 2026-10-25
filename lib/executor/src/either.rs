use core::future::Future;

/// Одна из двух future с общим выходом, без размещения в куче.
pub enum Either<L, R> {
    /// Первая future.
    Left(L),
    /// Вторая future.
    Right(R),
}

impl<L: Future, R: Future<Output = L::Output>> Either<L, R> {
    /// Ждёт future своего варианта.
    pub async fn run(self) -> L::Output {
        match self {
            Self::Left(future) => future.await,
            Self::Right(future) => future.await,
        }
    }
}
