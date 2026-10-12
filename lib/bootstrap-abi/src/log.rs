//! Фасад [`bootstrap::log`], усекающий строку до остатка кадра за тегом.

use ipc::{
    Channel,
    wire::{FIELD_DATA_MAX, FIELD_OVERHEAD, IpcError},
};

use crate::bootstrap;

impl<C: Channel> bootstrap::Client<C> {
    /// Шлёт [`bootstrap::log`], усекая `message` по границе символа до
    /// [`log_message_max`] для `tag`. Тег, который не помещается в кадр, даёт `FrameOverflow`.
    pub fn log_str(&self, tag: &str, message: &str) -> Result<(), IpcError> {
        let message = message
            .get(..message.floor_char_boundary(log_message_max(tag)))
            .unwrap_or_default();
        self.log(tag, message)
    }
}

/// Наибольшая длина строки [`bootstrap::log`] с тегом `tag`, то есть остаток кадра за тегом.
#[must_use]
pub const fn log_message_max(tag: &str) -> usize {
    FIELD_DATA_MAX
        .saturating_sub(FIELD_OVERHEAD)
        .saturating_sub(tag.len())
}

#[cfg(test)]
mod tests {
    use std::{format, string::String};

    use ipc_test::{MockHandle, MockResources};

    use crate::{bootstrap, log_message_max};

    #[derive(Default)]
    struct Sink {
        last_log: Option<String>,
    }

    impl crate::bootstrap::Service<MockResources> for Sink {
        fn log(&mut self, _frame: &MockResources, tag: &str, message: &str) {
            self.last_log = Some(format!("[{tag}] {message}"));
        }

        fn acquire_userland_image(
            &mut self,
            _frame: &MockResources,
        ) -> impl ipc::IntoWireValue<
            MockResources,
            Read = Result<ipc::wire::Cap<syscall::kind::MemoryRegion>, u32>,
        > {
            Err::<MockHandle<syscall::kind::MemoryRegion>, u32>(1)
        }

        fn acquire_hardware(
            &mut self,
            _frame: &MockResources,
        ) -> impl ipc::IntoWireValue<
            MockResources,
            Read = Result<ipc::wire::Session<crate::hardware::Protocol>, u32>,
        > {
            Err::<ipc::wire::Session<crate::hardware::Protocol, MockHandle<syscall::kind::Port>>, u32>(
                1,
            )
        }

        fn acquire_system_info(
            &mut self,
            _frame: &MockResources,
        ) -> impl ipc::IntoWireValue<
            MockResources,
            Read = Result<ipc::wire::Session<crate::system_info::Protocol>, u32>,
        > {
            Err::<
                ipc::wire::Session<crate::system_info::Protocol, MockHandle<syscall::kind::Port>>,
                u32,
            >(1)
        }

        fn acquire_log(
            &mut self,
            _frame: &MockResources,
        ) -> impl ipc::IntoWireValue<MockResources, Read = Result<log_abi::LogRing, u32>> {
            Err::<
                log_abi::LogRing<
                    MockHandle<syscall::kind::MemoryRegion>,
                    MockHandle<syscall::kind::Signal>,
                >,
                u32,
            >(1)
        }
    }

    #[test]
    fn log_str_round_trip() {
        let (client_end, server_end) = ipc_test::pair();
        let client = bootstrap::Client::new(client_end);
        client.log_str("rtc-client", "clock ok").expect("log write");
        let mut sink = Sink::default();
        server_end
            .serve(|frame| bootstrap::dispatch(&mut sink, frame))
            .expect("dispatch ok");
        assert_eq!(sink.last_log.as_deref(), Some("[rtc-client] clock ok"));
    }

    #[test]
    fn log_str_clamps_the_message_to_the_frame_rest() {
        let (client_end, server_end) = ipc_test::pair();
        let client = bootstrap::Client::new(client_end);
        let tag = "t".repeat(200);
        let long_message = "m".repeat(log_message_max(&tag) + 5);
        client
            .log_str(&tag, &long_message)
            .expect("clamped log write");
        let mut sink = Sink::default();
        server_end
            .serve(|frame| bootstrap::dispatch(&mut sink, frame))
            .expect("dispatch ok");
        let logged = sink.last_log.expect("logged");
        assert_eq!(logged.len(), 1 + tag.len() + 2 + log_message_max(&tag));
    }
}
