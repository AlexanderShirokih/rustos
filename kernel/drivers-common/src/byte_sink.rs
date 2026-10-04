/// Сигнал "пока занято, попробуй позже".
#[derive(Copy, Clone, Debug)]
pub struct Pending;

/// Неблокирующий приёмник байтов.
///
/// Позволяет записывать данные без ожидания готовности устройства.
/// При невозможности записи возвращает `Pending`.
pub trait ByteSink {
    /// Записать 1 байт без блокировок.
    fn try_write(&self, b: u8) -> Result<(), Pending>;

    /// Попытаться записать сразу несколько байт.
    /// Возвращает: Ok(n) - фактически записано n (может быть < buf.len()),
    /// Err(Pending) - не удалось записать ни одного байта.
    fn try_write_slice(&self, buf: &[u8]) -> Result<usize, Pending> {
        let mut n = 0;
        for &b in buf {
            match self.try_write(b) {
                Ok(()) => n += 1,
                Err(Pending) => break,
            }
        }
        if n == 0 { Err(Pending) } else { Ok(n) }
    }

    /// Дождаться полного опустошения буфера устройства.
    fn flush(&self) {}
}
