/// Сигнал «пока занято, попробуй позже»
#[derive(Copy, Clone, Debug)]
pub struct WouldBlock;

pub trait ByteSink {
    /// Записать 1 байт без блокировок.
    fn try_write(&self, b: u8) -> Result<(), WouldBlock>;

    /// Попытаться записать сразу несколько байт.
    /// Возвращает: Ok(n) — фактически записано n (может быть < buf.len()),
    /// Err(WouldBlock) — не удалось записать ни одного байта.
    #[inline(always)]
    fn try_write_slice(&self, buf: &[u8]) -> Result<usize, WouldBlock> {
        let mut n = 0;
        for &b in buf {
            match self.try_write(b) {
                Ok(()) => n += 1,
                Err(WouldBlock) => break,
            }
        }
        if n == 0 { Err(WouldBlock) } else { Ok(n) }
    }

    /// Дождаться полного опустошения передатчика (по умолчанию — no-op).
    #[inline(always)]
    fn flush(&self) {}
}
