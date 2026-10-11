//! Контракты выводов узлов устройств: [`pins`] выдаёт сессию [`device_pins`] выводов узла по
//! пути узла в описании и применяет к ним состояние `default` узла. [`default_states`] находит в
//! описании узлы этого состояния и их контроллеры, [`controllers`] - контроллеры состояния без
//! повторов, [`sets`] - наборы узла состояния. [`line()`] находит запись линии узла.
//! [`Claims`] ведёт занятость узлов и выводов сессиями поставщика.

#![cfg_attr(not(test), no_std)]

extern crate alloc;

mod claims;
mod error;
mod line;
mod protocol;
mod sets;
mod state;

pub use claims::{Claims, Pin};
pub use error::PinsError;
pub use line::{LineRecord, line};
pub use protocol::{device_pins, pins};
pub use sets::{check_state, pin_names, pin_number, sets};
pub use state::{PinState, controllers, default_states};
