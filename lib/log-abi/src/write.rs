//! Строковый фасад [`log::write`]: принимает `&str` и
//! укладывает их в кадры.

use ipc::{Channel, wire::IpcError};

use crate::{LOG_MESSAGE_MAX, chunks, log};

impl<C: Channel> log::Client<C> {
    /// Шлёт [`log::write`]. Сообщение длиннее кадра уходит несколькими кадрами.
    pub fn write_str(&self, message: &str) -> Result<(), IpcError> {
        for chunk in chunks::<LOG_MESSAGE_MAX>(message) {
            self.write(chunk)?;
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use std::{string::String, vec::Vec};

    use crate::{LOG_MESSAGE_MAX, log, tests::Sink};

    fn logged_frames(message: &str) -> Vec<String> {
        let (client_end, server_end) = ipc_test::pair();
        log::Client::new(client_end)
            .write_str(message)
            .expect("log write");
        let mut sink = Sink::default();
        let mut frames = Vec::new();
        server_end
            .serve_all(|frame| {
                log::dispatch(&mut sink, frame)?;
                frames.extend(sink.last.take());
                Ok::<_, ipc::wire::IpcError>(())
            })
            .expect("dispatch ok");
        frames
    }

    #[test]
    fn write_str_round_trip() {
        assert_eq!(logged_frames("node created"), ["node created"]);
    }

    #[test]
    fn write_str_splits_a_long_message_without_losing_it() {
        let message = "m".repeat(LOG_MESSAGE_MAX + 5);
        let frames = logged_frames(&message);
        assert_eq!(frames.len(), 2);
        assert_eq!(frames.concat(), message);
    }

    #[test]
    fn write_str_splits_on_a_char_boundary() {
        // Символы двухбайтовые, граница кадра приходится на середину одного из них.
        let message = "\u{451}".repeat(LOG_MESSAGE_MAX);
        assert_eq!(logged_frames(&message).concat(), message);
    }

    #[test]
    fn write_str_keeps_a_message_at_the_bound_in_one_frame() {
        let message = "m".repeat(LOG_MESSAGE_MAX);
        assert_eq!(logged_frames(&message).len(), 1);
    }
}
