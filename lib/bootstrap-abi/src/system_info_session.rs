//! Контракт [`system_info`] сведений ядра о системе.

ipc::wire_value! {
    /// Величины системы, прочитанные ядром при обработке одного вызова
    /// [`system_info::snapshot`].
    #[derive(Debug, Clone, Copy, PartialEq, Eq)]
    pub struct SystemSnapshot {
        /// Число CPU, на которые планировщик ставит потоки.
        pub cpus: u32,
        /// Число фреймов физической памяти под управлением аллокатора фреймов ядра.
        pub managed_frames: u64,
        /// Число свободных фреймов из управляемых.
        pub free_frames: u64,
        /// Размер фрейма в байтах.
        pub frame_size: u32,
    }
}

ipc::protocol! {
    /// Канал к сведениям ядра о системе.
    pub mod system_info {
        name = "SystemInfo";

        /// Снимок величин системы на момент обработки вызова.
        call fn snapshot() -> SystemSnapshot;
    }
}

#[cfg(test)]
mod tests {
    use ipc_test::MockResources;

    use super::{SystemSnapshot, system_info};

    const SNAPSHOT: SystemSnapshot = SystemSnapshot {
        cpus: 4,
        managed_frames: 0x2_0000_0000,
        free_frames: 0x1_8000_0000,
        frame_size: 4096,
    };

    struct Sink;

    impl system_info::Service<MockResources> for Sink {
        fn snapshot(&self, _frame: &MockResources) -> SystemSnapshot {
            SNAPSHOT
        }
    }

    #[test]
    fn snapshot_round_trip() {
        let (client_end, server_end) = ipc_test::pair();
        let client = system_info::Client::new(client_end);
        std::thread::scope(|scope| {
            scope.spawn(|| {
                server_end
                    .serve(|frame| system_info::dispatch(&Sink, frame))
                    .expect("dispatch ok");
            });
            assert_eq!(client.snapshot().wait(), Ok(SNAPSHOT));
        });
    }
}
