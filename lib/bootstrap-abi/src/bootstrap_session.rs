//! Контракт [`bootstrap`] первого процесса с ядром.

use syscall::kind;

use crate::{hardware, system_info};

ipc::protocol! {
    /// Канал начального процесса к ядру.
    pub mod bootstrap {
        name = "Bootstrap";

        /// Передаёт строку для логгера ядра klog. `tag` - источник строки
        /// (обычно имя процесса). Пустой тег - строка без тега.
        cast fn log(
            tag: &str,
            message: &str,
        );

        /// Выдаёт вызывателю read-only capability на регион поверх байт userland-образа.
        call fn acquire_userland_image() -> own Result<ipc::wire::Cap<kind::MemoryRegion>, u32>;

        /// Выдаёт вызывателю сессию [`hardware`]. Капа сессии разрешает отправку, перенос и
        /// дублирование и не разрешает приём. `Err` - ненулевой код ошибки выдачи.
        call fn acquire_hardware() -> own Result<ipc::wire::Session<hardware::Protocol>, u32>;

        /// Выдаёт вызывателю сессию [`system_info`]. Капа сессии разрешает отправку, перенос и
        /// дублирование и не разрешает приём. `Err` - ненулевой код ошибки выдачи.
        call fn acquire_system_info() -> own Result<ipc::wire::Session<system_info::Protocol>, u32>;

        /// Выдаёт вызывателю лог ядра: регион кольца с правами `READ`, `DUPLICATE` и `TRANSFER` и
        /// новый сигнал с правами `READ`, `WRITE`, `DUPLICATE` и `TRANSFER`. Ядро поднимает
        /// сигнал после каждой строки [`log`](self::log), пока у сигнала есть handle. `Err` -
        /// ядро не ведёт кольцо либо не выдало capability.
        call fn acquire_log() -> own Result<log_abi::LogRing, u32>;
    }
}

#[cfg(test)]
mod tests {
    use std::{format, string::String, thread};

    use ipc::{
        IntoWireValue,
        wire::{Cap, Session},
    };
    use ipc_test::{MockHandle, MockResources};
    use log_abi::LogRing;
    use syscall::{ObjectKind, kind};

    use super::bootstrap;
    use crate::{hardware, system_info};

    fn cap<K: ObjectKind>(raw: u32) -> MockHandle<K> {
        MockHandle::new(raw)
    }

    #[derive(Default)]
    struct Sink {
        last_log: Option<String>,
    }

    impl bootstrap::Service<MockResources> for Sink {
        fn log(&mut self, _frame: &MockResources, tag: &str, message: &str) {
            self.last_log = Some(format!("[{tag}] {message}"));
        }

        fn acquire_userland_image(
            &mut self,
            _frame: &MockResources,
        ) -> impl IntoWireValue<MockResources, Read = Result<Cap<kind::MemoryRegion>, u32>>
        {
            Ok::<_, u32>(cap::<kind::MemoryRegion>(0x88))
        }

        fn acquire_hardware(
            &mut self,
            _frame: &MockResources,
        ) -> impl IntoWireValue<MockResources, Read = Result<Session<hardware::Protocol>, u32>>
        {
            Ok::<_, u32>(Session::<hardware::Protocol, _>::from_handle(cap::<
                kind::Port,
            >(
                0x55
            )))
        }

        fn acquire_system_info(
            &mut self,
            _frame: &MockResources,
        ) -> impl IntoWireValue<MockResources, Read = Result<Session<system_info::Protocol>, u32>>
        {
            Ok::<_, u32>(Session::<system_info::Protocol, _>::from_handle(cap::<
                kind::Port,
            >(
                0x66
            )))
        }

        fn acquire_log(
            &mut self,
            _frame: &MockResources,
        ) -> impl IntoWireValue<MockResources, Read = Result<LogRing, u32>> {
            Ok::<_, u32>(LogRing {
                region: cap::<kind::MemoryRegion>(0x77),
                signal: cap::<kind::Signal>(0x78),
            })
        }
    }

    #[test]
    fn log_round_trip() {
        let (client_end, server_end) = ipc_test::pair();
        let client = bootstrap::Client::new(client_end);
        client.log("rootkeeper", "boot ok").expect("log write");
        let mut sink = Sink::default();
        server_end
            .serve(|frame| bootstrap::dispatch(&mut sink, frame))
            .expect("dispatch ok");
        assert_eq!(sink.last_log.as_deref(), Some("[rootkeeper] boot ok"));
    }

    #[test]
    fn bootstrap_acquisitions_answer_with_their_capability() {
        let (client_end, server_end) = ipc_test::pair();
        let client = bootstrap::Client::new(client_end);
        thread::scope(|scope| {
            scope.spawn(|| {
                let mut sink = Sink::default();
                for _ in 0..4 {
                    server_end
                        .serve(|frame| bootstrap::dispatch(&mut sink, frame))
                        .expect("dispatch ok");
                }
            });
            assert_eq!(client.acquire_userland_image(), Ok(cap(0x88)));
            assert_eq!(
                client.acquire_hardware().map(Session::into_handle),
                Ok(cap(0x55))
            );
            assert_eq!(
                client.acquire_system_info().map(Session::into_handle),
                Ok(cap(0x66))
            );
            assert_eq!(
                client.acquire_log(),
                Ok(LogRing {
                    region: cap(0x77),
                    signal: cap(0x78),
                })
            );
        });
    }
}
