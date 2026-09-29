//! Протокол шагов между `child-steps-fixture` и тестом. Фикстура запрашивает шаг вызовом `next`,
//! исполняет его и передаёт исход следующим вызовом `next`.

// Модуль подключён к прогону и к фикстуре, и каждый бинарь пользуется своим подмножеством.
#![allow(dead_code)]

use component_abi::{AcceptError, SessionError};
use ipc::wire::IpcError;
use syscall::Timeout;

pub const STEP_LINE: &str = "step logged";

pub mod action {
    pub const ACCEPT: u32 = 1;
    pub const ANSWER_SESSION: u32 = 2;
    pub const ANSWER_BADGED: u32 = 3;
    pub const ANSWER_REFUSAL: u32 = 4;
    pub const DROP_REQUEST: u32 = 5;
    pub const RECV: u32 = 6;
    pub const LOG: u32 = 7;
    pub const EXIT: u32 = 8;
    pub const WAIT: u32 = 9;
}

pub mod outcome {
    pub const STARTED: u32 = 1;
    pub const ACCEPTED: u32 = 2;
    pub const ACCEPT_REFUSED: u32 = 3;
    pub const IPC_FAILED: u32 = 4;
    pub const ANSWERED: u32 = 5;
    pub const NO_REQUEST: u32 = 6;
    pub const DROPPED: u32 = 7;
    pub const FRAME: u32 = 8;
    pub const SENDER_GONE: u32 = 9;
    pub const RECV_TIMEOUT: u32 = 10;
    pub const RECV_FAILED: u32 = 11;
    pub const LOGGED: u32 = 12;
    pub const NO_SESSION: u32 = 13;
    pub const UNKNOWN_STEP: u32 = 14;
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
    pub const fn accept(wait: Timeout) -> Self {
        Self::new(action::ACCEPT, wait.raw())
    }

    #[must_use]
    pub const fn answer_session() -> Self {
        Self::new(action::ANSWER_SESSION, 0)
    }

    #[must_use]
    pub const fn answer_badged(badge: u64) -> Self {
        Self::new(action::ANSWER_BADGED, badge)
    }

    #[must_use]
    pub const fn answer_refusal(refusal: SessionError) -> Self {
        Self::new(action::ANSWER_REFUSAL, session_error_code(refusal))
    }

    #[must_use]
    pub const fn drop_request() -> Self {
        Self::new(action::DROP_REQUEST, 0)
    }

    #[must_use]
    pub const fn recv(wait: Timeout) -> Self {
        Self::new(action::RECV, wait.raw())
    }

    #[must_use]
    pub const fn log() -> Self {
        Self::new(action::LOG, 0)
    }

    #[must_use]
    pub const fn exit(code: u32) -> Self {
        Self::new(action::EXIT, code as u64)
    }

    #[must_use]
    pub const fn wait() -> Self {
        Self::new(action::WAIT, 0)
    }

    const fn new(action: u32, argument: u64) -> Self {
        Self { action, argument }
    }
}

#[must_use]
pub const fn ipc_error_code(error: IpcError) -> u64 {
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
pub const fn accept_error_code(error: AcceptError) -> u64 {
    match error {
        AcceptError::NoSpace => 1,
        AcceptError::Refused => 2,
    }
}

#[must_use]
pub const fn session_error_code(error: SessionError) -> u64 {
    match error {
        SessionError::NoSpace => 1,
        SessionError::Refused => 2,
        SessionError::Unavailable => 3,
        SessionError::Unbound => 4,
    }
}

#[must_use]
pub const fn session_error_of(code: u64) -> Option<SessionError> {
    match code {
        1 => Some(SessionError::NoSpace),
        2 => Some(SessionError::Refused),
        3 => Some(SessionError::Unavailable),
        4 => Some(SessionError::Unbound),
        _ => None,
    }
}
