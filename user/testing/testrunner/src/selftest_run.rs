//! Запуск стенда библиотеки.
//!
//! Тестовый родитель запускает стенд, как любого ребёнка, со своей стартовой сессией и
//! журналом под бейджем стенда, ждёт ухода стенда и собирает строки, которые стенд вывел в
//! журнал. Бюджет стенда - образ программы и запас кучи сверх него.

use alloc::{string::String, vec::Vec};
use core::num::NonZeroU64;

use managed_component::{Departure, Outcome};
use rootkeeper::relay::LogSessions as _;
use runtime::Timeout;

use crate::{
    fixture::{process_bytes, program},
    log_relay::{
        ScenarioLogSessions, SharedSourceNames, await_component_lines, start_component_relay,
        stop_component_relay,
    },
    parent::{Answer, Child, Parent},
};

pub(crate) const PROCESS_TIMEOUT: Timeout = Timeout::from_secs(20);

const HEAP_SLACK_BYTES: u64 = 4 * 1024 * 1024;

pub(crate) struct Stand {
    pub(crate) entry: &'static str,
    pub(crate) name: &'static str,
    pub(crate) badge: NonZeroU64,
}

impl Stand {
    pub(crate) fn run(
        &self,
        require: impl FnOnce(Child) -> Child,
        outcome: Outcome,
        line_count: usize,
    ) -> Vec<String> {
        start_component_relay(SharedSourceNames);
        let log = ScenarioLogSessions
            .session(self.badge)
            .expect("selftest log session");
        let budget = process_bytes(&program(self.entry)) + HEAP_SLACK_BYTES;
        let parent = Parent::start();

        let child = parent.add(
            require(Child::new(self.entry, self.name))
                .log(Answer::log(log))
                .budget(budget),
        );

        test_harness::check_eq!(
            parent.departure(PROCESS_TIMEOUT),
            Some(Departure { child, outcome })
        );

        let lines = await_component_lines(self.name, line_count);
        stop_component_relay();
        lines
    }
}
