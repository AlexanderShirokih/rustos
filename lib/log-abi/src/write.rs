//! Строковый фасад [`Log::write`](crate::Log::write): принимает `&str` и
//! укладывает их в кадры.

use ipc::{
    Transport,
    wire::{IpcError, Str},
};

use crate::{LOG_MESSAGE_MAX, LogClient, chunks};

impl<T: Transport> LogClient<T> {
    /// Шлёт [`Log::write`](crate::Log::write). Сообщение длиннее кадра уходит
    /// несколькими кадрами под тем же тегом, тег усекается до своей границы.
    pub fn write_str(&self, tag: &str, message: &str) -> Result<(), IpcError> {
        for chunk in chunks::<LOG_MESSAGE_MAX>(message) {
            self.write(Str::truncating(tag), Str::truncating(chunk))?;
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use std::{format, string::String, vec::Vec};

    use ipc_test::MockEnd;

    use crate::{LOG_MESSAGE_MAX, LOG_TAG_MAX, LogClient, dispatch_log, tests::Sink};

    fn logged_frames(tag: &str, message: &str) -> Vec<String> {
        let (client_end, server_end) = MockEnd::pair();
        LogClient::new(client_end)
            .write_str(tag, message)
            .expect("log write");
        let mut sink = Sink::default();
        let mut frames = Vec::new();
        while dispatch_log(&mut sink, &server_end).is_ok() {
            match sink.last.take() {
                Some(line) => frames.push(line),
                None => break,
            }
        }
        frames
    }

    fn logged(tag: &str, message: &str) -> String {
        let frames = logged_frames(tag, message);
        assert_eq!(frames.len(), 1);
        frames.into_iter().next().expect("one frame")
    }

    #[test]
    fn write_str_round_trip() {
        assert_eq!(logged("ramfs", "node created"), "[ramfs] node created");
    }

    #[test]
    fn write_str_splits_a_long_message_without_losing_it() {
        let message = "m".repeat(LOG_MESSAGE_MAX + 5);
        let frames = logged_frames("tag", &message);
        assert_eq!(frames.len(), 2);
        let carried: String = frames.iter().map(|line| &line["[tag] ".len()..]).collect();
        assert_eq!(carried, message);
    }

    #[test]
    fn write_str_splits_on_a_char_boundary() {
        // Символы двухбайтовые, граница кадра приходится на середину одного из них.
        let message = "\u{451}".repeat(LOG_MESSAGE_MAX);
        let frames = logged_frames("tag", &message);
        let carried: String = frames.iter().map(|line| &line["[tag] ".len()..]).collect();
        assert_eq!(carried, message);
    }

    #[test]
    fn write_str_keeps_a_message_at_the_bound_in_one_frame() {
        let message = "m".repeat(LOG_MESSAGE_MAX);
        assert_eq!(logged_frames("tag", &message).len(), 1);
    }

    #[test]
    fn write_str_clamps_the_tag_to_its_bound() {
        let long_tag = "t".repeat(LOG_TAG_MAX + 5);
        let line = logged(&long_tag, "line");
        assert_eq!(line.len(), 1 + LOG_TAG_MAX + 2 + "line".len());
    }

    #[test]
    fn write_str_cuts_the_tag_on_a_char_boundary() {
        // Тег из двухбайтовых символов не делится границей LOG_TAG_MAX ровно.
        let long_tag = "é".repeat(LOG_TAG_MAX);
        let line = logged(&long_tag, "line");
        assert_eq!(line, format!("[{}] line", "é".repeat(LOG_TAG_MAX / 2)));
    }
}
