//! Строковый фасад [`bootstrap::log`]: принимает `&str`
//! и усекает их до границ кадра, снимая с вызывателя конструирование `Str`.

use ipc::{
    Channel,
    wire::{IpcError, Str},
};

use crate::bootstrap;

impl<C: Channel> bootstrap::Client<C> {
    /// Шлёт [`bootstrap::log`], усекая `tag` и
    /// `message` до границ кадра по границе символа.
    pub fn log_str(&self, tag: &str, message: &str) -> Result<(), IpcError> {
        self.log(Str::truncating(tag), Str::truncating(message))
    }
}

#[cfg(test)]
mod tests {
    use std::{format, string::String};

    use ipc::wire::Str;

    use crate::{LOG_MESSAGE_MAX, LOG_TAG_MAX, bootstrap};

    #[derive(Default)]
    struct Sink {
        last_log: Option<String>,
    }

    impl<F: ipc::ReceivedResources> crate::bootstrap::Service<F> for Sink {
        fn log(
            &mut self,
            _frame: &F,
            tag: ipc::wire::Str<'_, { LOG_TAG_MAX }>,
            message: ipc::wire::Str<'_, { LOG_MESSAGE_MAX }>,
        ) {
            self.last_log = Some(format!("[{}] {}", tag.as_str(), message.as_str()));
        }

        fn acquire_irq_control(
            &mut self,
            _frame: &F,
        ) -> Result<ipc::wire::Cap<crate::kind::IrqControl>, u32> {
            Err(1)
        }

        fn acquire_userland_image(
            &mut self,
            _frame: &F,
        ) -> Result<ipc::wire::Cap<crate::kind::MemoryRegion>, u32> {
            Err(1)
        }

        fn acquire_boot_fdt(&mut self, _frame: &F) -> Result<crate::BootFdt, u32> {
            Err(1)
        }

        fn acquire_device_memory(
            &mut self,
            _frame: &F,
            _index: u32,
        ) -> Result<ipc::wire::Cap<crate::kind::MemoryRegion>, u32> {
            Err(1)
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
    fn log_str_clamps_overlong_strings() {
        let (client_end, server_end) = ipc_test::pair();
        let client = bootstrap::Client::new(client_end);
        let long_tag = "t".repeat(LOG_TAG_MAX + 5);
        let long_message = "m".repeat(LOG_MESSAGE_MAX + 5);
        client
            .log_str(&long_tag, &long_message)
            .expect("clamped log write");
        let mut sink = Sink::default();
        server_end
            .serve(|frame| bootstrap::dispatch(&mut sink, frame))
            .expect("dispatch ok");
        let logged = sink.last_log.expect("logged");
        assert_eq!(logged.len(), 1 + LOG_TAG_MAX + 2 + LOG_MESSAGE_MAX);
    }

    #[test]
    fn truncating_respects_char_boundary() {
        // "é" занимает два байта, срез по середине символа запрещён.
        assert_eq!(Str::<4>::truncating("ééé").as_str(), "éé");
        assert_eq!(Str::<4>::truncating("abc").as_str(), "abc");
    }
}
