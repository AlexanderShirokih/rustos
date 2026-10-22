//! Состояние ввода Doom: кадры контактов, захват площадок и объединение удержаний.

use alloc::collections::BTreeMap;

use input_abi::{AxisMode, Binding, DeviceLayout, DeviceNumber, Kind, Record, Usage};

use crate::{
    Button, Control, Layout, Mode, Point,
    key::{
        KEY_DOWNARROW, KEY_FIRE, KEY_LEFTARROW, KEY_RIGHTARROW, KEY_RSHIFT, KEY_STRAFE_L,
        KEY_STRAFE_R, KEY_UPARROW, doom_key,
    },
};

/// Переход игровой клавиши в кодах `doomkeys.h`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct KeyEvent {
    pub key: u8,
    pub pressed: bool,
}

/// Снимок контроллера для рисунка. Состояние меню задаёт поток движка.
#[derive(Debug, Clone, Copy)]
pub struct Controls {
    pub mode: Mode,
    pub weapons: bool,
    pub held: [bool; 256],
    pub stick: Option<Point>,
}

/// Обрабатывает целые кадры `Input`. Дескрипторы запрашиваются при `DeviceAdded`.
/// `Resync`, уход устройства и `release_all` отпускают его действия с учётом остальных
/// источников. Новый контакт выбирает площадку на первом `Frame` и держит её до конца.
pub struct InputState {
    layout: Layout,
    devices: BTreeMap<DeviceNumber, Device>,
    held: [bool; 256],
    mode: Mode,
    weapons: bool,
    repeat: Option<(u8, u64)>,
    stick: Option<Point>,
}

struct Device {
    layout: DeviceLayout,
    keys: BTreeMap<u16, u8>,
    contacts: BTreeMap<u16, Contact>,
}

#[derive(Default)]
struct Contact {
    x: Option<u32>,
    y: Option<u32>,
    capture: Option<(Option<Button>, Point)>,
}

impl InputState {
    pub const fn new(layout: Layout) -> Self {
        Self {
            layout,
            devices: BTreeMap::new(),
            held: [false; 256],
            mode: Mode::Game,
            weapons: false,
            repeat: None,
            stick: None,
        }
    }

    /// Смена назначения требует нового касания. Физические клавиши сохраняются.
    pub fn set_mode(&mut self, mode: Mode, mut emit: impl FnMut(KeyEvent)) {
        if self.mode == mode {
            return;
        }
        self.mode = mode;
        self.weapons = false;
        self.repeat = None;
        for device in self.devices.values_mut() {
            device.contacts.clear();
        }
        self.publish(&mut emit);
    }

    pub fn controls(&self) -> Controls {
        Controls {
            mode: self.mode,
            weapons: self.weapons,
            held: self.held,
            stick: self.stick,
        }
    }

    /// Повтор стрелки меню после 350 мс удержания, затем каждые 120 мс.
    /// `now` - монотонные миллисекунды вызывающего потока.
    pub fn tick(&mut self, now: u64, mut emit: impl FnMut(KeyEvent)) {
        let key = matches!(self.mode, Mode::Menu | Mode::MainMenu)
            .then(|| {
                [KEY_UPARROW, KEY_DOWNARROW, KEY_LEFTARROW, KEY_RIGHTARROW]
                    .into_iter()
                    .find(|&key| self.held.get(usize::from(key)) == Some(&true))
            })
            .flatten();
        match (key, self.repeat) {
            (Some(key), Some((old, deadline))) if key == old => {
                if now >= deadline {
                    emit(KeyEvent { key, pressed: true });
                    self.repeat = Some((key, now.saturating_add(120)));
                }
            }
            (Some(key), _) => self.repeat = Some((key, now.saturating_add(350))),
            (None, _) => self.repeat = None,
        }
    }

    /// Передаёт переходы после `Frame`, `DeviceRemoved` и `Resync`. `describe` вызывается
    /// только при появлении устройства. Отсутствие дескриптора пропускает устройство.
    pub fn feed(
        &mut self,
        record: Record,
        describe: impl FnOnce(DeviceNumber) -> Option<DeviceLayout>,
        mut emit: impl FnMut(KeyEvent),
    ) {
        let Some(number) = record.device else {
            if record.kind == Kind::Resync {
                self.release_all(emit);
            }
            return;
        };
        match record.kind {
            Kind::DeviceAdded => {
                if let Some(layout) = describe(number) {
                    self.devices.insert(
                        number,
                        Device {
                            layout,
                            keys: BTreeMap::new(),
                            contacts: BTreeMap::new(),
                        },
                    );
                }
            }
            Kind::DeviceRemoved => {
                self.devices.remove(&number);
                self.publish(&mut emit);
            }
            Kind::Frame => self.publish(&mut emit),
            _ => {
                if let Some(device) = self.devices.get_mut(&number) {
                    device.feed(record, self.layout);
                }
            }
        }
    }

    /// Снимает все устройства, например при отказе чтения сессии.
    pub fn release_all(&mut self, mut emit: impl FnMut(KeyEvent)) {
        self.devices.clear();
        self.weapons = false;
        self.repeat = None;
        self.publish(&mut emit);
    }

    fn publish(&mut self, emit: &mut impl FnMut(KeyEvent)) {
        let mut held = [false; 256];
        let mut press = |key: u8| {
            if let Some(slot) = held.get_mut(usize::from(key)) {
                *slot = true;
            }
        };
        let mut toggle = false;
        for device in self.devices.values_mut() {
            for &key in device.keys.values() {
                press(key);
            }
            for contact in device.contacts.values_mut() {
                toggle |= contact.press(self.layout, self.mode, self.weapons, &mut press);
            }
        }
        self.stick = self
            .devices
            .values()
            .flat_map(|d| d.contacts.values())
            .find_map(|c| {
                if c.capture
                    .is_some_and(|(b, _)| b.is_some_and(|b| b.control == Control::Move))
                {
                    Some(self.layout.logical(Point { x: c.x?, y: c.y? }))
                } else {
                    None
                }
            });
        if toggle {
            self.weapons = !self.weapons;
        }
        if (b'1'..=b'7').any(|key| held.get(usize::from(key)) == Some(&true)) {
            self.weapons = false;
        }
        if [KEY_UPARROW, KEY_DOWNARROW, KEY_LEFTARROW, KEY_RIGHTARROW]
            .iter()
            .any(|&key| held.get(usize::from(key)) != self.held.get(usize::from(key)))
        {
            self.repeat = None;
        }
        for (key, (previous, &next)) in (0u8..=255).zip(self.held.iter_mut().zip(&held)) {
            if *previous != next {
                *previous = next;
                emit(KeyEvent { key, pressed: next });
            }
        }
    }
}

impl Device {
    fn feed(&mut self, record: Record, layout: Layout) {
        match record.kind {
            Kind::Key => {
                if record.value == 0 {
                    self.keys.remove(&record.code);
                } else if let Some(key) = self.layout.key(record.code).and_then(doom_key) {
                    self.keys.insert(record.code, key);
                }
            }
            Kind::ContactDown if self.layout.binding() == Binding::Screen => {
                self.contacts.insert(record.contact, Contact::default());
            }
            Kind::ContactUp => {
                self.contacts.remove(&record.contact);
            }
            Kind::Axis => {
                let Some(contact) = self.contacts.get_mut(&record.contact) else {
                    return;
                };
                let Some(axis) = self.layout.contact_axis(record.code) else {
                    return;
                };
                let AxisMode::Absolute { min, max } = axis.mode else {
                    return;
                };
                let panel = layout.panel();
                let (coordinate, length) = match axis.usage {
                    Usage::X => (&mut contact.x, panel.width),
                    Usage::Y => (&mut contact.y, panel.height),
                    _ => return,
                };
                let offset = (record.value.clamp(i64::from(min), i64::from(max)) - i64::from(min))
                    .unsigned_abs();
                let span = u64::from(max.abs_diff(min)).max(1);
                *coordinate = u32::try_from(offset * u64::from(length - 1) / span).ok();
            }
            _ => {}
        }
    }
}

impl Contact {
    fn press(
        &mut self,
        layout: Layout,
        mode: Mode,
        weapons: bool,
        press: &mut impl FnMut(u8),
    ) -> bool {
        let (Some(x), Some(y)) = (self.x, self.y) else {
            return false;
        };
        let point = layout.logical(Point { x, y });
        let initial = self.capture.is_none();
        let (button, origin) = *self.capture.get_or_insert_with(|| {
            (
                layout
                    .buttons(mode, weapons)
                    .into_iter()
                    .find(|b| b.contains(point)),
                point,
            )
        });
        let Some(button) = button else {
            return false;
        };
        let dead = i64::from((layout.height / 36).max(1));
        match button.control {
            Control::Weapons => return initial,
            Control::Key(key) => press(key),
            Control::Move => {
                let x = i64::from(point.x) - i64::from(button.area.x + button.area.width / 2);
                let y = i64::from(point.y) - i64::from(button.area.y + button.area.height / 2);
                if matches!(mode, Mode::Menu | Mode::MainMenu) {
                    if x.abs() > y.abs() {
                        direction(x, dead, KEY_LEFTARROW, KEY_RIGHTARROW, press);
                    } else {
                        direction(y, dead, KEY_UPARROW, KEY_DOWNARROW, press);
                    }
                } else if mode == Mode::Game {
                    direction(x, dead, KEY_STRAFE_L, KEY_STRAFE_R, press);
                    direction(y, dead, KEY_UPARROW, KEY_DOWNARROW, press);
                    if x.abs().max(y.abs()) > i64::from(button.area.width) * 2 / 5 {
                        press(KEY_RSHIFT);
                    }
                }
            }
            Control::Turn | Control::Fire => {
                if button.control == Control::Fire {
                    press(KEY_FIRE);
                }
                direction(
                    i64::from(point.x) - i64::from(origin.x),
                    dead,
                    KEY_LEFTARROW,
                    KEY_RIGHTARROW,
                    press,
                );
            }
        }
        false
    }
}

fn direction(value: i64, dead: i64, negative: u8, positive: u8, press: &mut impl FnMut(u8)) {
    if value < -dead {
        press(negative);
    }
    if value > dead {
        press(positive);
    }
}
