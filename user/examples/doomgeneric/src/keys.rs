//! Клавиши клавиатуры сессии `Input` в кодах движка doomgeneric.
//!
//! Поток чтения забирает записи сессии и ставит в очередь нажатия и отпускания клавиш страницы
//! клавиатуры HID в кодах `doomkeys.h`. Перевод следует `convertToDoomKey` порта
//! `doomgeneric_sdl.c`: Ctrl - выстрел, пробел - использование, Shift - бег, Alt - стрейф,
//! буквы, цифры и знаки - строчные символы ASCII. Backspace, F1 и F12 получают свои коды
//! `doomkeys.h`. Клавиши без кода пропускаются.
//!
//! Поток держит нажатые клавиши каждого устройства. `Resync` и уход устройства отпускают его
//! нажатые клавиши. Отказ `next` пишется в журнал, отпускает все нажатые клавиши и завершает
//! поток.

use alloc::collections::{BTreeMap, VecDeque, btree_map::Entry};

use input_abi::{DeviceLayout, Kind, Record, Usage, input};
use kernel_objects::{Session, SessionTransport};
use runtime::{Mutex, log_fmt, spawn_owned};
use syscall::SyscallError;

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

const KEY_ENTER: u8 = 13;
const KEY_ESCAPE: u8 = 27;
const KEY_BACKSPACE: u8 = 0x7F;
const KEY_TAB: u8 = 9;

const KEY_RIGHTARROW: u8 = 0xAE;
const KEY_LEFTARROW: u8 = 0xAC;
const KEY_DOWNARROW: u8 = 0xAF;
const KEY_UPARROW: u8 = 0xAD;

const KEY_USE: u8 = 0xA2;
const KEY_FIRE: u8 = 0xA3;
const KEY_RSHIFT: u8 = 0xB6;
const KEY_LALT: u8 = 0xB8;

static EVENTS: Mutex<VecDeque<KeyEvent>> = Mutex::new(VecDeque::new());

pub(crate) struct KeyEvent {
    pub(crate) pressed: bool,
    pub(crate) key: u8,
}

#[derive(Default)]
struct Keyboard {
    devices: BTreeMap<u16, Device>,
}

struct Device {
    layout: DeviceLayout,
    held: BTreeMap<u16, u8>,
}

impl Keyboard {
    fn feed(&mut self, record: Record, describe: impl FnOnce(u16) -> Option<DeviceLayout>) {
        match record.kind {
            Kind::Resync => self.release_all(),
            Kind::DeviceAdded => {
                if let Some(layout) = describe(record.device) {
                    let device = Device {
                        layout,
                        held: BTreeMap::new(),
                    };
                    self.devices.insert(record.device, device);
                }
            }
            Kind::DeviceRemoved => {
                if let Some(device) = self.devices.remove(&record.device) {
                    device.release();
                }
            }
            Kind::Key => {
                if let Some(device) = self.devices.get_mut(&record.device) {
                    device.key(record.code, record.value != 0);
                }
            }
            _ => {}
        }
    }

    fn release_all(&mut self) {
        for device in core::mem::take(&mut self.devices).into_values() {
            device.release();
        }
    }
}

impl Device {
    fn key(&mut self, code: u16, pressed: bool) {
        if !pressed {
            if let Some(key) = self.held.remove(&code) {
                push([KeyEvent { pressed, key }]);
            }
            return;
        }

        if let Entry::Vacant(entry) = self.held.entry(code)
            && let Some(key) = self.layout.key(code).and_then(doom_key)
        {
            entry.insert(key);
            push([KeyEvent { pressed, key }]);
        }
    }

    fn release(self) {
        push(self.held.into_values().map(|key| KeyEvent {
            pressed: false,
            key,
        }));
    }
}

pub(crate) fn start(session: Session<input::Protocol>) -> Result<(), SyscallError> {
    let client = session.client();
    spawn_owned(move || {
        read(&client);
        0
    })
    .map(drop)
}

pub(crate) fn next() -> Option<KeyEvent> {
    EVENTS.lock().pop_front()
}

fn read(client: &input::Client<SessionTransport>) {
    let mut keyboard = Keyboard::default();
    loop {
        let records = match client.next() {
            Ok(records) => records,
            Err(error) => {
                log_fmt(format_args!("input stopped: {error:?}"));
                keyboard.release_all();
                return;
            }
        };
        for record in records.records() {
            keyboard.feed(record, |device| client.describe(device).ok());
        }
    }
}

fn push(events: impl IntoIterator<Item = KeyEvent>) {
    EVENTS.lock().extend(events);
}

fn doom_key(usage: Usage) -> Option<u8> {
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
