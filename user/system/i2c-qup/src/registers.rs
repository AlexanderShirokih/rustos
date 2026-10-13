//! Смещения регистров окна контроллера QUP.

pub(crate) const CONFIG: usize = 0x000;
pub(crate) const STATE: usize = 0x004;
pub(crate) const IO_MODE: usize = 0x008;
pub(crate) const SW_RESET: usize = 0x00c;
pub(crate) const OPERATIONAL: usize = 0x018;
pub(crate) const ERROR_FLAGS: usize = 0x01c;
pub(crate) const MX_OUTPUT_CNT: usize = 0x100;
pub(crate) const OUTPUT_FIFO: usize = 0x110;
pub(crate) const MX_WRITE_CNT: usize = 0x150;
pub(crate) const MX_INPUT_CNT: usize = 0x200;
pub(crate) const MX_READ_CNT: usize = 0x208;
pub(crate) const INPUT_FIFO: usize = 0x218;
pub(crate) const I2C_CLK_CTL: usize = 0x400;
pub(crate) const I2C_STATUS: usize = 0x404;
pub(crate) const I2C_MASTER_GEN: usize = 0x408;

pub(crate) const WINDOW_MIN: usize = I2C_MASTER_GEN + 4;
