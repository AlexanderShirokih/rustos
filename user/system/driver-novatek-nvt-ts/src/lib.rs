//! Разбирает параметры NT36672A и преобразует отчёты касаний в команды хаба ввода.
//!
//! [`Parameters::parse`] проверяет параметры чипа, [`Device::new`] задаёт устройство и
//! диапазоны осей, а [`Contacts::translate`] формирует команды и обновляет состояние
//! контактов. Формат параметров и записей отчёта описан в
//! `drivers/input/touchscreen/novatek-nvt-ts.c` Linux 6.16.
#![no_std]

mod device;
mod parameters;
mod report;

pub use device::{Device, DeviceError};
pub use parameters::{PARAMETERS_ADDRESS, PARAMETERS_LENGTH, Parameters, ParametersError};
pub use report::{AXIS_X, AXIS_Y, Contacts, REPORT_ADDRESS};
