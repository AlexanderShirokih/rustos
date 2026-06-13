//! Bootstrap-контракт: канал init процесса к ядру.

#![cfg_attr(not(test), no_std)]

#[cfg(test)]
extern crate std;

/// Граница строки [`Bootstrap::log`].
pub const LOG_MESSAGE_MAX: usize = ipc::wire::FIELD_DATA_MAX;

#[ipc::protocol(name = "Bootstrap")]
pub trait Bootstrap {
    /// Передаёт строку для логгера ядра klog.
    #[cast]
    fn log(&self, message: ipc::wire::Str<{ crate::LOG_MESSAGE_MAX }>);
}

#[cfg(test)]
mod tests {
    use std::string::{String, ToString};

    use ipc::wire::Str;
    use ipc_test::MockEnd;

    use super::{BootstrapClient, BootstrapService, LOG_MESSAGE_MAX, dispatch_bootstrap};

    #[derive(Default)]
    struct Sink {
        last_log: Option<String>,
    }

    impl BootstrapService for Sink {
        fn log(&mut self, message: Str<{ LOG_MESSAGE_MAX }>) {
            self.last_log = Some(message.as_str().to_string());
        }
    }

    #[test]
    fn log_round_trip() {
        let (client_end, server_end) = MockEnd::pair();
        let client = BootstrapClient::new(client_end);
        client
            .log(Str::<LOG_MESSAGE_MAX>::new("boot ok").expect("within bound"))
            .expect("log write");
        let mut sink = Sink::default();
        dispatch_bootstrap(&mut sink, &server_end).expect("dispatch ok");
        assert_eq!(sink.last_log.as_deref(), Some("boot ok"));
    }
}
