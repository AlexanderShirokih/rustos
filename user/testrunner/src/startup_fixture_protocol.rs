//! Startup-протокол фикстуры стандартного запуска процесса.

/// Значение, подтверждающее доставку стартового кадра.
pub const STARTUP_VALUE: u64 = 0x51A7_7EAD;

ipc::protocol! {
    /// Контракт стартовой фикстуры.
    pub mod startup_fixture {
        name = "StartupFixture";

        /// Передаёт стартовое значение процессу-фикстуре.
        cast fn start(value: u64);
    }
}

impl startup_fixture::Service for Option<u64> {
    fn start(&mut self, value: u64) {
        *self = Some(value);
    }
}
