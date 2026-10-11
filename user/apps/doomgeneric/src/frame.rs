//! Размер кадра движка в пикселях, равный родному разрешению Doom. Движок выводит кадр в
//! `DG_ScreenBuffer` этого размера без собственного масштаба.

pub(crate) const FRAME_WIDTH: u32 = 320;
pub(crate) const FRAME_HEIGHT: u32 = 200;
