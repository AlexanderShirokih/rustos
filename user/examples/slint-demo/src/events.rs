//! Перевод записей сессии `Input` в события окна Slint.
//!
//! Указатель окна ведут устройства с привязкой осей к экрану: абсолютные оси `X` и `Y`
//! устройства с кнопками указателя либо первый контакт касания. Клавиши с аналогом среди
//! клавиш Slint приходят окну нажатием и отпусканием. `Resync` и уход устройства снимают
//! нажатие указателя без клика, а нажатия из снимка устройства начинают жест заново.

use std::collections::BTreeMap;

use input_abi::{AxisMode, Binding, CANCEL, DeviceLayout, Kind, NO_CONTACT, Record, Usage};
use slint::{
    LogicalPosition, SharedString,
    platform::{Key, PointerEventButton, WindowEvent},
};

#[derive(Clone, Copy)]
pub(crate) struct Screen {
    pub(crate) width: f32,
    pub(crate) height: f32,
    pub(crate) scale: f32,
}

pub(crate) struct Translator {
    screen: Screen,
    devices: BTreeMap<u16, Device>,
    touch: Option<(u16, u16)>,
    events: Vec<WindowEvent>,
}

struct Device {
    layout: DeviceLayout,
    pointer: Point,
    pointer_moved: bool,
    buttons: Vec<(PointerEventButton, bool)>,
    buttons_held: u8,
    contacts: BTreeMap<u16, Contact>,
}

#[derive(Default, Clone, Copy)]
struct Point {
    x: Option<f32>,
    y: Option<f32>,
}

struct Contact {
    point: Point,
    phase: Phase,
    moved: bool,
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum Phase {
    Down,
    Held,
    Up { cancel: bool },
}

impl Translator {
    pub(crate) const fn new(screen: Screen) -> Self {
        Self {
            screen,
            devices: BTreeMap::new(),
            touch: None,
            events: Vec::new(),
        }
    }

    pub(crate) fn feed(
        &mut self,
        record: Record,
        describe: impl FnOnce(u16) -> Option<DeviceLayout>,
    ) {
        match record.kind {
            Kind::Resync => {
                let held =
                    self.touch.is_some() || self.devices.values().any(|d| d.buttons_held > 0);
                if held {
                    self.events.push(WindowEvent::PointerExited);
                }
                self.devices.clear();
                self.touch = None;
            }
            Kind::DeviceAdded => {
                if let Some(layout) = describe(record.device) {
                    self.devices.insert(record.device, Device::new(layout));
                }
            }
            Kind::DeviceRemoved => {
                let held = self
                    .touch
                    .is_some_and(|(device, _)| device == record.device)
                    || self
                        .devices
                        .get(&record.device)
                        .is_some_and(|d| d.buttons_held > 0);
                if held {
                    self.events.push(WindowEvent::PointerExited);
                    self.touch = None;
                }
                self.devices.remove(&record.device);
            }
            Kind::Key => self.key(record),
            Kind::Axis => {
                if let Some(device) = self.devices.get_mut(&record.device) {
                    device.axis(record, self.screen);
                }
            }
            Kind::ContactDown => {
                if let Some(device) = self.devices.get_mut(&record.device) {
                    device.contacts.insert(
                        record.contact,
                        Contact {
                            point: Point::default(),
                            phase: Phase::Down,
                            moved: false,
                        },
                    );
                }
            }
            Kind::ContactUp => {
                if let Some(contact) = self
                    .devices
                    .get_mut(&record.device)
                    .and_then(|d| d.contacts.get_mut(&record.contact))
                {
                    contact.phase = Phase::Up {
                        cancel: record.flags & CANCEL != 0,
                    };
                }
            }
            Kind::Frame => self.frame(record.device),
            Kind::Unknown(_) => {}
        }
    }

    pub(crate) fn take_events(&mut self) -> Vec<WindowEvent> {
        core::mem::take(&mut self.events)
    }

    fn key(&mut self, record: Record) {
        let Some(device) = self.devices.get_mut(&record.device) else {
            return;
        };
        let pressed = record.value != 0;
        let Some(usage) = device.layout.key(record.code) else {
            return;
        };
        if device.layout.binding() == Binding::Screen
            && let Some(button) = pointer_button(usage)
        {
            device.buttons.push((button, pressed));
            return;
        }

        if let Some(key) = slint_key(usage) {
            let text = SharedString::from(key);
            self.events.push(if pressed {
                WindowEvent::KeyPressed { text }
            } else {
                WindowEvent::KeyReleased { text }
            });
        }
    }

    fn frame(&mut self, id: u16) {
        let Some(device) = self.devices.get_mut(&id) else {
            return;
        };
        let scale = self.screen.scale;

        if let Some(position) = device.pointer.position(scale) {
            if device.pointer_moved {
                self.events.push(WindowEvent::PointerMoved { position });
            }
            for (button, pressed) in device.buttons.drain(..) {
                if pressed {
                    device.buttons_held = device.buttons_held.saturating_add(1);
                    self.events
                        .push(WindowEvent::PointerPressed { position, button });
                } else if device.buttons_held > 0 {
                    device.buttons_held -= 1;
                    self.events
                        .push(WindowEvent::PointerReleased { position, button });
                }
            }
        }
        device.buttons.clear();
        device.pointer_moved = false;

        for (&number, contact) in &mut device.contacts {
            let Some(position) = contact.point.position(scale) else {
                continue;
            };
            if self.touch.is_none() && contact.phase == Phase::Down {
                self.touch = Some((id, number));
                self.events.push(WindowEvent::PointerMoved { position });
                self.events.push(WindowEvent::PointerPressed {
                    position,
                    button: PointerEventButton::Left,
                });
                contact.moved = false;
            }
            if self.touch != Some((id, number)) {
                continue;
            }

            if contact.moved {
                self.events.push(WindowEvent::PointerMoved { position });
            }
            if let Phase::Up { cancel } = contact.phase {
                if !cancel {
                    self.events.push(WindowEvent::PointerReleased {
                        position,
                        button: PointerEventButton::Left,
                    });
                }
                self.events.push(WindowEvent::PointerExited);
                self.touch = None;
            }
        }
        device
            .contacts
            .retain(|_, contact| !matches!(contact.phase, Phase::Up { .. }));
        for contact in device.contacts.values_mut() {
            contact.moved = false;
            if contact.phase == Phase::Down {
                contact.phase = Phase::Held;
            }
        }
    }
}

impl Device {
    const fn new(layout: DeviceLayout) -> Self {
        Self {
            layout,
            pointer: Point { x: None, y: None },
            pointer_moved: false,
            buttons: Vec::new(),
            buttons_held: 0,
            contacts: BTreeMap::new(),
        }
    }

    fn axis(&mut self, record: Record, screen: Screen) {
        if self.layout.binding() != Binding::Screen {
            return;
        }

        let (decl, point) = if record.contact == NO_CONTACT {
            (self.layout.device_axis(record.code), &mut self.pointer)
        } else {
            let Some(contact) = self.contacts.get_mut(&record.contact) else {
                return;
            };
            contact.moved = true;
            (self.layout.contact_axis(record.code), &mut contact.point)
        };
        let Some(decl) = decl else {
            return;
        };
        let AxisMode::Absolute { min, max } = decl.mode else {
            return;
        };

        if decl.usage == Usage::X {
            point.x = Some(screen_coordinate(record.value, min, max, screen.width));
        } else if decl.usage == Usage::Y {
            point.y = Some(screen_coordinate(record.value, min, max, screen.height));
        } else {
            return;
        }
        if record.contact == NO_CONTACT {
            self.pointer_moved = true;
        }
    }
}

impl Point {
    fn position(self, scale: f32) -> Option<LogicalPosition> {
        Some(LogicalPosition::new(self.x? / scale, self.y? / scale))
    }
}

#[allow(clippy::cast_precision_loss, reason = "screen coordinates fit f32")]
fn screen_coordinate(value: i64, min: i32, max: i32, length: f32) -> f32 {
    let span = i64::from(max) - i64::from(min);
    if span <= 0 {
        return 0.0;
    }
    let offset = value.clamp(i64::from(min), i64::from(max)) - i64::from(min);
    offset as f32 / span as f32 * (length - 1.0).max(0.0)
}

fn pointer_button(usage: Usage) -> Option<PointerEventButton> {
    match usage {
        Usage::PRIMARY => Some(PointerEventButton::Left),
        Usage::SECONDARY => Some(PointerEventButton::Right),
        Usage::MIDDLE => Some(PointerEventButton::Middle),
        _ => None,
    }
}

fn slint_key(usage: Usage) -> Option<Key> {
    match usage {
        Usage::ENTER => Some(Key::Return),
        Usage::ESCAPE => Some(Key::Escape),
        Usage::BACKSPACE => Some(Key::Backspace),
        Usage::TAB => Some(Key::Tab),
        Usage::RIGHT => Some(Key::RightArrow),
        Usage::LEFT => Some(Key::LeftArrow),
        Usage::DOWN => Some(Key::DownArrow),
        Usage::UP => Some(Key::UpArrow),
        _ => None,
    }
}
