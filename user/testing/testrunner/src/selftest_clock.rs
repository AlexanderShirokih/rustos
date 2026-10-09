//! Часы `Rtc` стендов библиотек.
//!
//! Поток теста отвечает на `now` заданным временем, пока не ответит один раз. Модуль выдаёт
//! ответ на требование `rtc`, диапазон времени суток, которое ребёнок мог отсчитать от этих
//! часов к концу прогона, и разбирает строку `time N` с выведенным временем.

use core::ops::RangeInclusive;

use ipc::{Listener as _, Protocol as _, RecvError, wire::MESSAGE_INLINE_MAX};
use kernel_objects::{Delivery, Port};
use rtc_abi::rtc;
use runtime::{Instant, JoinHandle, PortTransport, Timeout, spawn_owned};
use syscall::{Handle, Rights, kind};

use crate::parent::Answer;

pub(crate) const RTC_REQUIREMENT: &str = "rtc";

const RTC_SECONDS: u64 = 1_760_000_000;

const NS_PER_SEC: u64 = 1_000_000_000;

const TIME_PREFIX: &str = "time ";

pub(crate) struct SelftestClock {
    port: Port,
    server: JoinHandle,
    started: Instant,
    timeout: Timeout,
}

#[derive(Default)]
struct Clock {
    answered: bool,
}

impl SelftestClock {
    pub(crate) fn start(timeout: Timeout) -> Self {
        let port = Port::create().expect("rtc port");
        let server_port = port.as_raw();
        let server = spawn_owned(move || serve_until_now_answered(server_port, timeout))
            .expect("rtc server starts");

        Self {
            port,
            server,
            started: Instant::now(),
            timeout,
        }
    }

    pub(crate) fn answer(&self) -> Answer {
        Answer::Grant {
            protocol: rtc::Protocol::ID,
            source: self
                .port
                .duplicate(Rights::WRITE | Rights::TRANSFER | Rights::DUPLICATE, None)
                .expect("rtc source"),
        }
    }

    pub(crate) fn finish(self) -> RangeInclusive<u64> {
        let elapsed_seconds = Instant::now()
            .since(self.started)
            .raw()
            .div_ceil(NS_PER_SEC);
        self.server.wait(self.timeout).expect("rtc server ends");
        test_harness::check_eq!(self.server.exit_code().expect("rtc server exit code"), 1);
        self.server.join(Timeout::POLL).expect("rtc server joins");

        RTC_SECONDS..=RTC_SECONDS + elapsed_seconds + 1
    }
}

impl rtc::Service<Delivery> for Clock {
    fn now(&mut self, _frame: &Delivery) -> u64 {
        self.answered = true;
        RTC_SECONDS
    }

    fn tick(&mut self, _frame: &Delivery) -> u64 {
        RTC_SECONDS
    }
}

pub(crate) fn printed_time(line: &str) -> Option<u64> {
    line.strip_prefix(TIME_PREFIX)?.parse().ok()
}

fn serve_until_now_answered(port: Handle<kind::Port>, timeout: Timeout) -> u32 {
    let mut clock = Clock::default();
    let mut bytes = [0u8; MESSAGE_INLINE_MAX];
    while !clock.answered {
        match PortTransport::new(port).recv(&mut bytes, timeout) {
            Ok(incoming) => {
                let _ = rtc::dispatch(&mut clock, incoming);
            }
            Err(RecvError::SenderGone(_) | RecvError::Discarded) => {}
            Err(RecvError::Timeout | RecvError::Closed) => break,
        }
    }

    u32::from(clock.answered)
}
