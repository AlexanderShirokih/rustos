//! Контракт [`journal`] выдаёт подписчику регион кольца лога ядра и сигнал новых записей.
//! Формат кольца задают [`RingWriter`](crate::RingWriter) и [`RingReader`](crate::RingReader).

ipc::wire_enum! {
    /// Отказ в подписке на журнал.
    #[derive(Debug, Clone, Copy, PartialEq, Eq)]
    pub enum JournalError {
        /// Подписка не выдана.
        Refused = 1,
    }
}

ipc::protocol! {
    /// Подписка на строки системного журнала.
    pub mod journal {
        name = "Journal";

        /// Выдаёт кольцо лога ядра со строками ядра, rootkeeper и компонентов. Регион
        /// отображается только на чтение, сигнал сбрасывается подписчиком. Incarnation
        /// получает одну подписку, её регион и сигнал действуют до конца incarnation.
        call fn subscribe() -> own Result<crate::LogRing, JournalError>;
    }
}

#[cfg(test)]
mod tests {
    use core::cell::Cell;
    use std::thread;

    use ipc::{CallError, IntoWireValue, wire::Cap};
    use ipc_test::{MockHandle, MockResources};
    use syscall::kind;

    use super::{JournalError, journal};
    use crate::LogRing;

    struct Subscriptions {
        granted: Cell<bool>,
    }

    impl journal::Service<MockResources> for Subscriptions {
        fn subscribe(
            &self,
            _frame: &MockResources,
        ) -> impl IntoWireValue<
            MockResources,
            Read = Result<LogRing<Cap<kind::MemoryRegion>, Cap<kind::Signal>>, JournalError>,
        > {
            if self.granted.replace(true) {
                return Err(JournalError::Refused);
            }
            Ok(LogRing {
                region: MockHandle::<kind::MemoryRegion>::new(0x41),
                signal: MockHandle::<kind::Signal>::new(0x42),
            })
        }
    }

    #[test]
    fn subscribe_round_trips_the_ring_and_the_refusal() {
        let (client_end, server_end) = ipc_test::pair();
        let client = journal::Client::new(client_end);
        let service = Subscriptions {
            granted: Cell::new(false),
        };
        thread::scope(|scope| {
            scope.spawn(move || {
                server_end
                    .serve_all(|frame| journal::dispatch(&service, frame))
                    .expect("dispatch ok");
            });
            assert_eq!(
                client.subscribe().wait(),
                Ok(LogRing {
                    region: MockHandle::new(0x41),
                    signal: MockHandle::new(0x42),
                })
            );
            assert_eq!(
                client.subscribe().wait(),
                Err(CallError::Remote(JournalError::Refused))
            );
            drop(client);
        });
    }
}
