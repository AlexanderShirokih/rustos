//! Протокол шагов между `child-steps-fixture` и тестом. Фикстура запрашивает шаг вызовом `next`,
//! исполняет его и передаёт исход следующим вызовом `next`.

// Модуль подключён к прогону и к фикстуре, и каждый бинарь пользуется своим подмножеством.
#![allow(dead_code)]

use component_abi::{AcceptError, SessionError};
use ipc::wire::IpcError;
use syscall::Timeout;

pub(crate) const STEP_LINE: &str = "step logged";
pub(crate) const STEP_PANIC: &str = "step panics";

pub(crate) mod action {
    pub(crate) const ACCEPT: u32 = 1;
    pub(crate) const ANSWER_SESSION: u32 = 2;
    pub(crate) const ANSWER_BADGED: u32 = 3;
    pub(crate) const ANSWER_REFUSAL: u32 = 4;
    pub(crate) const DROP_REQUEST: u32 = 5;
    pub(crate) const RECV: u32 = 6;
    pub(crate) const LOG: u32 = 7;
    pub(crate) const EXIT: u32 = 8;
    pub(crate) const WAIT: u32 = 9;
    pub(crate) const RETURN_OK: u32 = 10;
    pub(crate) const RETURN_ERR: u32 = 11;
    pub(crate) const PANIC: u32 = 12;
}

pub(crate) mod outcome {
    pub(crate) const STARTED: u32 = 1;
    pub(crate) const ACCEPTED: u32 = 2;
    pub(crate) const ACCEPT_REFUSED: u32 = 3;
    pub(crate) const IPC_FAILED: u32 = 4;
    pub(crate) const ANSWERED: u32 = 5;
    pub(crate) const NO_REQUEST: u32 = 6;
    pub(crate) const DROPPED: u32 = 7;
    pub(crate) const FRAME: u32 = 8;
    pub(crate) const SENDER_GONE: u32 = 9;
    pub(crate) const RECV_TIMEOUT: u32 = 10;
    pub(crate) const RECV_FAILED: u32 = 11;
    pub(crate) const LOGGED: u32 = 12;
    pub(crate) const NO_SESSION: u32 = 13;
    pub(crate) const UNKNOWN_STEP: u32 = 14;
}

ipc::wire_value! {
    #[derive(Debug, Clone, Copy, PartialEq, Eq)]
    pub struct Step {
        pub action: u32,
        pub argument: u64,
    }
}

ipc::wire_value! {
    #[derive(Debug, Clone, Copy, PartialEq, Eq)]
    pub struct Report {
        pub outcome: u32,
        pub value: u64,
    }
}

ipc::protocol! {
    pub mod child_steps {
        name = "ChildSteps";

        call(defer) fn next(report: Report) -> Step;
    }
}

impl Step {
    #[must_use]
    pub(crate) const fn accept(wait: Timeout) -> Self {
        Self::new(action::ACCEPT, wait.raw())
    }

    #[must_use]
    pub(crate) const fn answer_session() -> Self {
        Self::new(action::ANSWER_SESSION, 0)
    }

    #[must_use]
    pub(crate) const fn answer_badged(badge: u64) -> Self {
        Self::new(action::ANSWER_BADGED, badge)
    }

    #[must_use]
    pub(crate) const fn answer_refusal(refusal: SessionError) -> Self {
        Self::new(action::ANSWER_REFUSAL, session_error_code(refusal))
    }

    #[must_use]
    pub(crate) const fn drop_request() -> Self {
        Self::new(action::DROP_REQUEST, 0)
    }

    #[must_use]
    pub(crate) const fn recv(wait: Timeout) -> Self {
        Self::new(action::RECV, wait.raw())
    }

    #[must_use]
    pub(crate) const fn log() -> Self {
        Self::new(action::LOG, 0)
    }

    #[must_use]
    pub(crate) const fn exit(code: u32) -> Self {
        Self::new(action::EXIT, code as u64)
    }

    #[must_use]
    pub(crate) const fn wait() -> Self {
        Self::new(action::WAIT, 0)
    }

    #[must_use]
    pub(crate) const fn return_ok() -> Self {
        Self::new(action::RETURN_OK, 0)
    }

    #[must_use]
    pub(crate) const fn return_err() -> Self {
        Self::new(action::RETURN_ERR, 0)
    }

    #[must_use]
    pub(crate) const fn panic() -> Self {
        Self::new(action::PANIC, 0)
    }

    const fn new(action: u32, argument: u64) -> Self {
        Self { action, argument }
    }
}

#[must_use]
pub(crate) const fn ipc_error_code(error: IpcError) -> u64 {
    match error {
        IpcError::Truncated => 1,
        IpcError::FrameOverflow => 2,
        IpcError::FieldOrder => 3,
        IpcError::MissingField => 4,
        IpcError::InvalidValue => 5,
        IpcError::PeerClosed => 6,
        IpcError::Timeout => 7,
        IpcError::TooManyHandles => 8,
        IpcError::ResourceExhausted => 9,
    }
}

#[must_use]
pub(crate) const fn accept_error_code(error: AcceptError) -> u64 {
    match error {
        AcceptError::NoSpace => 1,
        AcceptError::Refused => 2,
    }
}

#[must_use]
pub(crate) const fn session_error_code(error: SessionError) -> u64 {
    match error {
        SessionError::NoSpace => 1,
        SessionError::Refused => 2,
        SessionError::Unavailable => 3,
        SessionError::Unbound => 4,
    }
}

#[must_use]
pub(crate) const fn session_error_of(code: u64) -> Option<SessionError> {
    match code {
        1 => Some(SessionError::NoSpace),
        2 => Some(SessionError::Refused),
        3 => Some(SessionError::Unavailable),
        4 => Some(SessionError::Unbound),
        _ => None,
    }
}
