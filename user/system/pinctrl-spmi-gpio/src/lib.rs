//! Логика контроллера GPIO PMIC Qualcomm `qcom,spmi-gpio`: число выводов по совместимости,
//! раскладка регистров блока вывода по его подтипу, применение наборов состояния `default` и
//! линии-входы. Раскладка повторяет `drivers/pinctrl/qcom/pinctrl-spmi-gpio.c` Linux 6.12.
#![no_std]

extern crate alloc;

mod controller;
mod gpio;
mod line;
mod settings;

pub use controller::ControllerNode;
pub use gpio::{Gpio, Registers};
pub use line::{Line, line};
pub use settings::{Plan, Setting, plan};
