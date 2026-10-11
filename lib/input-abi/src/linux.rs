//! Перевод кодов событий Linux в [`Usage`].

use crate::usage::Usage;

const KEYBOARD_PAGE: u16 = 0x07;
const BUTTON_PAGE: u16 = 0x09;

const BTN_LEFT: u32 = 0x110;
const BTN_EXTRA: u32 = 0x114;

const REL_X: u32 = 0x00;
const REL_Y: u32 = 0x01;
const REL_HWHEEL: u32 = 0x06;
const REL_WHEEL: u32 = 0x08;

const ABS_X: u32 = 0x00;
const ABS_Y: u32 = 0x01;

#[rustfmt::skip]
const LINUX_KEY_OF_USAGE: [u8; 0xE8] = [
      0,  0,  0,  0, 30, 48, 46, 32, 18, 33, 34, 35, 23, 36, 37, 38,
     50, 49, 24, 25, 16, 19, 31, 20, 22, 47, 17, 45, 21, 44,  2,  3,
      4,  5,  6,  7,  8,  9, 10, 11, 28,  1, 14, 15, 57, 12, 13, 26,
     27, 43, 43, 39, 40, 41, 51, 52, 53, 58, 59, 60, 61, 62, 63, 64,
     65, 66, 67, 68, 87, 88, 99, 70,119,110,102,104,111,107,109,106,
    105,108,103, 69, 98, 55, 74, 78, 96, 79, 80, 81, 75, 76, 77, 71,
     72, 73, 82, 83, 86,127,116,117,183,184,185,186,187,188,189,190,
    191,192,193,194,134,138,130,132,128,129,131,137,133,135,136,113,
    115,114,  0,  0,  0,121,  0, 89, 93,124, 92, 94, 95,  0,  0,  0,
    122,123, 90, 91, 85,  0,  0,  0,  0,  0,  0,  0,111,  0,  0,  0,
      0,  0,  0,  0,  0,  0,  0,  0,  0,  0,  0,  0,  0,  0,  0,  0,
      0,  0,  0,  0,  0,  0,179,180,  0,  0,  0,  0,  0,  0,  0,  0,
      0,  0,  0,  0,  0,  0,  0,  0,  0,  0,  0,  0,  0,  0,  0,  0,
      0,  0,  0,  0,  0,  0,  0,  0,111,  0,  0,  0,  0,  0,  0,  0,
     29, 42, 56,125, 97, 54,100,126,
];

impl Usage {
    /// Переводит код `EV_KEY` Linux: клавишу - в usage страницы клавиатуры с наименьшим
    /// номером по таблице HID Linux, `BTN_LEFT`..=`BTN_EXTRA` - в кнопки 1-5.
    pub fn from_linux_key(code: u32) -> Option<Self> {
        if (BTN_LEFT..=BTN_EXTRA).contains(&code) {
            let button = u16::try_from(code - BTN_LEFT + 1).ok()?;
            return Some(Self::new(BUTTON_PAGE, button));
        }
        let key = u8::try_from(code).ok().filter(|key| *key != 0)?;
        let id = LINUX_KEY_OF_USAGE
            .iter()
            .position(|mapped| *mapped == key)?;
        Some(Self::new(KEYBOARD_PAGE, u16::try_from(id).ok()?))
    }

    /// Переводит код `EV_REL` Linux: X, Y, колесо и горизонтальное колесо.
    pub fn from_linux_rel(code: u32) -> Option<Self> {
        match code {
            REL_X => Some(Self::X),
            REL_Y => Some(Self::Y),
            REL_WHEEL => Some(Self::new(0x01, 0x38)),
            REL_HWHEEL => Some(Self::new(0x0C, 0x238)),
            _ => None,
        }
    }

    /// Переводит код `EV_ABS` Linux: X и Y.
    pub fn from_linux_abs(code: u32) -> Option<Self> {
        match code {
            ABS_X => Some(Self::X),
            ABS_Y => Some(Self::Y),
            _ => None,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn key_and_button_are_translated() {
        assert_eq!(Usage::from_linux_key(28), Some(Usage::ENTER));
        assert_eq!(Usage::from_linux_key(0x111), Some(Usage::SECONDARY));
    }

    #[test]
    fn platform_buttons_are_the_named_usages() {
        assert_eq!(Usage::from_linux_key(116), Some(Usage::POWER));
        assert_eq!(Usage::from_linux_key(102), Some(Usage::HOME));
        assert_eq!(Usage::from_linux_key(114), Some(Usage::VOLUME_DOWN));
        assert_eq!(Usage::from_linux_key(115), Some(Usage::VOLUME_UP));
    }

    #[test]
    fn code_without_translation_is_none() {
        assert_eq!(Usage::from_linux_key(0), None);
        assert_eq!(Usage::from_linux_key(240), None);
        assert_eq!(Usage::from_linux_key(0x115), None);
        assert_eq!(Usage::from_linux_rel(0x09), None);
        assert_eq!(Usage::from_linux_abs(0x02), None);
    }

    #[test]
    fn wheel_is_translated() {
        assert_eq!(
            Usage::from_linux_rel(REL_WHEEL),
            Some(Usage::new(0x01, 0x38))
        );
    }
}
