//! Строковый фасад [`Bootstrap::log`](crate::Bootstrap::log): принимает `&str`
//! и усекает их до границ кадра, снимая с вызывателя конструирование `Str`.

use ipc::{
    Transport,
    wire::{IpcError, Str},
};

use crate::{BootstrapClient, LOG_MESSAGE_MAX, LOG_TAG_MAX};

/// Префикс `text` длиной не более `max` байт по границе символа.
fn clamp(text: &str, max: usize) -> &str {
    if text.len() <= max {
        return text;
    }
    let mut len = max;
    while !text.is_char_boundary(len) {
        len -= 1;
    }
    &text[..len]
}

impl<T: Transport> BootstrapClient<T> {
    /// Шлёт [`Bootstrap::log`](crate::Bootstrap::log), усекая `tag` и
    /// `message` до границ кадра по границе символа.
    pub fn log_str(&self, tag: &str, message: &str) -> Result<(), IpcError> {
        let tag = Str::new(clamp(tag, LOG_TAG_MAX)).expect("clamped to LOG_TAG_MAX");
        let message =
            Str::new(clamp(message, LOG_MESSAGE_MAX)).expect("clamped to LOG_MESSAGE_MAX");
        self.log(tag, message)
    }
}

#[cfg(test)]
mod tests {
    use std::{format, string::String};

    use ipc_test::MockEnd;

    use super::clamp;
    use crate::{BootstrapClient, LOG_MESSAGE_MAX, LOG_TAG_MAX, dispatch_bootstrap};

    #[derive(Default)]
    struct Sink {
        last_log: Option<String>,
    }

    impl crate::BootstrapService for Sink {
        fn log(
            &mut self,
            tag: ipc::wire::Str<'_, { LOG_TAG_MAX }>,
            message: ipc::wire::Str<'_, { LOG_MESSAGE_MAX }>,
        ) {
            self.last_log = Some(format!("[{}] {}", tag.as_str(), message.as_str()));
        }

        fn acquire_irq_control(&mut self) -> Result<ipc::wire::Cap, u32> {
            Err(1)
        }

        fn acquire_userland_image(&mut self) -> Result<ipc::wire::Cap, u32> {
            Err(1)
        }

        fn acquire_boot_fdt(&mut self) -> Result<ipc::wire::Cap, u32> {
            Err(1)
        }

        fn acquire_device_memory(&mut self, _index: u32) -> Result<ipc::wire::Cap, u32> {
            Err(1)
        }
    }

    #[test]
    fn log_str_round_trip() {
        let (client_end, server_end) = MockEnd::pair();
        let client = BootstrapClient::new(client_end);
        client.log_str("rtc-client", "clock ok").expect("log write");
        let mut sink = Sink::default();
        dispatch_bootstrap(&mut sink, &server_end).expect("dispatch ok");
        assert_eq!(sink.last_log.as_deref(), Some("[rtc-client] clock ok"));
    }

    #[test]
    fn log_str_clamps_overlong_strings() {
        let (client_end, server_end) = MockEnd::pair();
        let client = BootstrapClient::new(client_end);
        let long_tag = "t".repeat(LOG_TAG_MAX + 5);
        let long_message = "m".repeat(LOG_MESSAGE_MAX + 5);
        client
            .log_str(&long_tag, &long_message)
            .expect("clamped log write");
        let mut sink = Sink::default();
        dispatch_bootstrap(&mut sink, &server_end).expect("dispatch ok");
        let logged = sink.last_log.expect("logged");
        assert_eq!(logged.len(), 1 + LOG_TAG_MAX + 2 + LOG_MESSAGE_MAX);
    }

    #[test]
    fn clamp_respects_char_boundary() {
        // "ё" - два байта; срез по середине символа запрещён.
        assert_eq!(clamp("ааа", 4), "аа");
        assert_eq!(clamp("abc", 4), "abc");
    }
}
