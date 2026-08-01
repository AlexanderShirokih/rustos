//! Startup-протокол фикстуры стандартного запуска процесса.

/// Значение, подтверждающее доставку стартового кадра.
pub const STARTUP_VALUE: u64 = 0x51A7_7EAD;

#[ipc::protocol(name = "StartupFixture")]
pub trait StartupFixture {
    /// Передаёт стартовое значение процессу-фикстуре.
    #[cast]
    fn start(&self, value: u64);
}

impl StartupFixtureService for Option<u64> {
    fn start(&mut self, value: u64) {
        *self = Some(value);
    }
}
