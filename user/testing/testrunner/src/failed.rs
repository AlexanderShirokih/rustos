//! Отказ фикстуры, код выхода которой не содержит данных.

use core::fmt;

#[derive(Debug)]
pub(crate) struct Failed;

impl fmt::Display for Failed {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str("failed")
    }
}
