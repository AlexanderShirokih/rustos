//! Коды `doomkeys.h` и перевод клавиатурных usage в них.

use input_abi::Usage;

const KEYBOARD_PAGE: u16 = 0x07;

const SPACE: Usage = Usage::new(KEYBOARD_PAGE, 0x2C);
const LEFT_CONTROL: Usage = Usage::new(KEYBOARD_PAGE, 0xE0);
const LEFT_SHIFT: Usage = Usage::new(KEYBOARD_PAGE, 0xE1);
const LEFT_ALT: Usage = Usage::new(KEYBOARD_PAGE, 0xE2);
const RIGHT_CONTROL: Usage = Usage::new(KEYBOARD_PAGE, 0xE4);
const RIGHT_SHIFT: Usage = Usage::new(KEYBOARD_PAGE, 0xE5);
const RIGHT_ALT: Usage = Usage::new(KEYBOARD_PAGE, 0xE6);

const CHARACTERS_FIRST_USAGE: u16 = 0x04;
const CHARACTERS: &[u8; 0x35] = b"abcdefghijklmnopqrstuvwxyz1234567890\0\0\0\0\0-=[]\\\0;'`,./";

const FUNCTION_KEYS_FIRST_USAGE: u16 = 0x3A;
const FUNCTION_KEYS: [u8; 12] = [
    0xBB, 0xBC, 0xBD, 0xBE, 0xBF, 0xC0, 0xC1, 0xC2, 0xC3, 0xC4, 0xD7, 0xD8,
];

pub(crate) const KEY_ENTER: u8 = 13;
pub(crate) const KEY_ESCAPE: u8 = 27;
pub(crate) const KEY_BACKSPACE: u8 = 0x7F;
pub(crate) const KEY_TAB: u8 = 9;

pub(crate) const KEY_RIGHTARROW: u8 = 0xAE;
pub(crate) const KEY_LEFTARROW: u8 = 0xAC;
pub(crate) const KEY_DOWNARROW: u8 = 0xAF;
pub(crate) const KEY_UPARROW: u8 = 0xAD;

pub(crate) const KEY_USE: u8 = 0xA2;
pub(crate) const KEY_FIRE: u8 = 0xA3;
pub(crate) const KEY_RSHIFT: u8 = 0xB6;
pub(crate) const KEY_LALT: u8 = 0xB8;

pub(crate) const KEY_STRAFE_L: u8 = 0xA0;
pub(crate) const KEY_STRAFE_R: u8 = 0xA1;

pub(crate) fn doom_key(usage: Usage) -> Option<u8> {
    let key = match usage {
        Usage::ENTER => KEY_ENTER,
        Usage::ESCAPE => KEY_ESCAPE,
        Usage::BACKSPACE => KEY_BACKSPACE,
        Usage::TAB => KEY_TAB,

        Usage::RIGHT => KEY_RIGHTARROW,
        Usage::LEFT => KEY_LEFTARROW,
        Usage::DOWN => KEY_DOWNARROW,
        Usage::UP => KEY_UPARROW,

        SPACE => KEY_USE,
        LEFT_CONTROL | RIGHT_CONTROL => KEY_FIRE,
        LEFT_SHIFT | RIGHT_SHIFT => KEY_RSHIFT,
        LEFT_ALT | RIGHT_ALT => KEY_LALT,

        Usage {
            page: KEYBOARD_PAGE,
            id,
        } => return table_key(id),
        _ => return None,
    };
    Some(key)
}

fn table_key(id: u16) -> Option<u8> {
    let character = id
        .checked_sub(CHARACTERS_FIRST_USAGE)
        .and_then(|index| CHARACTERS.get(usize::from(index)));
    let function = id
        .checked_sub(FUNCTION_KEYS_FIRST_USAGE)
        .and_then(|index| FUNCTION_KEYS.get(usize::from(index)));
    character.or(function).copied().filter(|key| *key != 0)
}
