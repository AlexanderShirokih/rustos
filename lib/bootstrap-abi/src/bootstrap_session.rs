//! Контракт [`bootstrap`] первого процесса с ядром и границы строк его журнала.

use syscall::kind;

use crate::hardware;

/// Граница тега источника в [`bootstrap::log`].
pub const LOG_TAG_MAX: usize = 64;

/// Граница строки [`bootstrap::log`]: остаток кадра за тегом.
pub const LOG_MESSAGE_MAX: usize =
    ipc::wire::FIELD_DATA_MAX - LOG_TAG_MAX - ipc::wire::FIELD_OVERHEAD;

ipc::protocol! {
    /// Канал начального процесса к ядру.
    pub mod bootstrap {
        name = "Bootstrap";

        /// Передаёт строку для логгера ядра klog. `tag` - источник строки
        /// (обычно имя процесса). Пустой тег - строка без тега.
        cast fn log(
            tag: ipc::wire::Str<{ LOG_TAG_MAX }>,
            message: ipc::wire::Str<{ LOG_MESSAGE_MAX }>,
        );

        /// Выдаёт вызывателю read-only capability на регион поверх байт userland-образа.
        call fn acquire_userland_image() -> own Result<ipc::wire::Cap<kind::MemoryRegion>, u32>;

        /// Выдаёт вызывателю сессию [`hardware`]. Капа сессии разрешает отправку, перенос и
        /// дублирование и не разрешает приём. `Err` - ненулевой код ошибки выдачи.
        call fn acquire_hardware() -> own Result<ipc::wire::Session<hardware::Protocol>, u32>;
    }
}

#[cfg(test)]
mod tests {
    use std::{format, string::String, thread};

    use ipc::{
        IntoWireValue,
        wire::{Cap, Session, Str},
    };
    use ipc_test::{MockHandle, MockResources};
    use syscall::{ObjectKind, kind};

    use super::{LOG_MESSAGE_MAX, LOG_TAG_MAX, bootstrap};
    use crate::hardware;

    fn cap<K: ObjectKind>(raw: u32) -> MockHandle<K> {
        MockHandle::new(raw)
    }

    #[derive(Default)]
    struct Sink {
        last_log: Option<String>,
    }

    impl bootstrap::Service<MockResources> for Sink {
        fn log(
            &mut self,
            _frame: &MockResources,
            tag: Str<'_, { LOG_TAG_MAX }>,
            message: Str<'_, { LOG_MESSAGE_MAX }>,
        ) {
            self.last_log = Some(format!("[{}] {}", tag.as_str(), message.as_str()));
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
    }

    #[test]
    fn log_round_trip() {
        let (client_end, server_end) = ipc_test::pair();
        let client = bootstrap::Client::new(client_end);
        client
            .log(
                Str::new("rootkeeper").expect("tag within bound"),
                Str::new("boot ok").expect("message within bound"),
            )
            .expect("log write");
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
                for _ in 0..2 {
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
        });
    }
}
