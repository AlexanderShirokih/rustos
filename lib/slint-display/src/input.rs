//! Перевод записей сессии `Input` в события окна Slint и их доставка окну.
//!
//! Указатель окна ведут устройства с привязкой осей к экрану: абсолютные оси `X` и `Y`
//! устройства с кнопками указателя либо первый контакт касания. Клавиши с аналогом среди
//! клавиш Slint и печатные клавиши раскладки US приходят окну нажатием и отпусканием.
//! Цифровой блок печатает цифры независимо от Num Lock. `Resync` и уход устройства снимают
//! нажатие указателя без клика, а нажатия из снимка устройства начинают жест заново.

use std::{collections::BTreeMap, rc::Rc};

use executor::{Executor, PortChannel};
use input_abi::{
    AxisMode, Binding, CANCEL, DeviceLayout, DeviceNumber, Kind, NO_CONTACT, Record, Usage, input,
};
use ipc::Timeout;
use kernel_objects::{Session, SessionTransport};
use slint::{
    ComponentHandle, LogicalPosition, SharedString,
    platform::{Key, PointerEventButton, WindowEvent},
};

use crate::screen::ScreenPlatform;

const KEYBOARD_PAGE: u16 = 0x07;
const LEFT_SHIFT: Usage = Usage::new(KEYBOARD_PAGE, 0xE1);
const RIGHT_SHIFT: Usage = Usage::new(KEYBOARD_PAGE, 0xE5);
const KEYPAD_ENTER: Usage = Usage::new(KEYBOARD_PAGE, 0x58);

const TYPED_FIRST: u16 = 0x04;
const TYPED: &[u8; 53] = b"abcdefghijklmnopqrstuvwxyz1234567890\0\0\0\0 -=[]\\\\;'`,./";
const TYPED_SHIFTED: &[u8; 53] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZ!@#$%^&*()\0\0\0\0 _+{}||:\"~<>?";

const KEYPAD_FIRST: u16 = 0x54;
const KEYPAD: &[u8; 16] = b"/*-+\x001234567890.";

#[derive(Clone, Copy)]
struct Screen {
    width: f32,
    height: f32,
    scale: f32,
}

struct Translator {
    screen: Screen,
    devices: BTreeMap<DeviceNumber, Device>,
    touch: Option<Touch>,
    events: Vec<WindowEvent>,
}

#[derive(Clone, Copy, PartialEq, Eq)]
struct Touch {
    device: DeviceNumber,
    contact: u16,
}

struct Device {
    layout: DeviceLayout,
    pointer: Point,
    pointer_moved: bool,
    buttons: Vec<ButtonChange>,
    buttons_held: u8,
    shifts_held: u8,
    contacts: BTreeMap<u16, Contact>,
}

struct ButtonChange {
    button: PointerEventButton,
    pressed: bool,
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
    const fn new(screen: Screen) -> Self {
        Self {
            screen,
            devices: BTreeMap::new(),
            touch: None,
            events: Vec::new(),
        }
    }

    fn feed(
        &mut self,
        record: Record,
        describe: impl FnOnce(DeviceNumber) -> Option<DeviceLayout>,
    ) {
        match (record.kind, record.device) {
            (Kind::Resync, _) => {
                let held =
                    self.touch.is_some() || self.devices.values().any(|d| d.buttons_held > 0);
                if held {
                    self.events.push(WindowEvent::PointerExited);
                }
                self.devices.clear();
                self.touch = None;
            }
            (Kind::DeviceAdded, Some(number)) => {
                if let Some(layout) = describe(number) {
                    self.devices.insert(number, Device::new(layout));
                }
            }
            (Kind::DeviceRemoved, Some(number)) => {
                let held = self.touch.is_some_and(|touch| touch.device == number)
                    || self
                        .devices
                        .get(&number)
                        .is_some_and(|d| d.buttons_held > 0);
                if held {
                    self.events.push(WindowEvent::PointerExited);
                    self.touch = None;
                }
                self.devices.remove(&number);
            }
            (Kind::Key, Some(number)) => self.key(number, record),
            (Kind::Axis, Some(number)) => {
                if let Some(device) = self.devices.get_mut(&number) {
                    device.axis(record, self.screen);
                }
            }
            (Kind::ContactDown, Some(number)) => {
                if let Some(device) = self.devices.get_mut(&number) {
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
            (Kind::ContactUp, Some(number)) => {
                if let Some(contact) = self
                    .devices
                    .get_mut(&number)
                    .and_then(|d| d.contacts.get_mut(&record.contact))
                {
                    contact.phase = Phase::Up {
                        cancel: record.flags & CANCEL != 0,
                    };
                }
            }
            (Kind::Frame, Some(number)) => self.frame(number),
            _ => {}
        }
    }

    fn take_events(&mut self) -> Vec<WindowEvent> {
        core::mem::take(&mut self.events)
    }

    fn key(&mut self, number: DeviceNumber, record: Record) {
        let Some(device) = self.devices.get_mut(&number) else {
            return;
        };
        let pressed = record.value != 0;
        let Some(usage) = device.layout.key(record.code) else {
            return;
        };
        if device.layout.binding() == Binding::Screen
            && let Some(button) = pointer_button(usage)
        {
            device.buttons.push(ButtonChange { button, pressed });
            return;
        }

        if usage == LEFT_SHIFT || usage == RIGHT_SHIFT {
            device.shifts_held = if pressed {
                device.shifts_held.saturating_add(1)
            } else {
                device.shifts_held.saturating_sub(1)
            };
        }

        let text = slint_key(usage)
            .map(SharedString::from)
            .or_else(|| typed(usage, device.shifts_held > 0).map(SharedString::from));
        if let Some(text) = text {
            self.events.push(if pressed {
                WindowEvent::KeyPressed { text }
            } else {
                WindowEvent::KeyReleased { text }
            });
        }
    }

    fn frame(&mut self, id: DeviceNumber) {
        let Some(device) = self.devices.get_mut(&id) else {
            return;
        };
        let scale = self.screen.scale;

        if let Some(position) = device.pointer.position(scale) {
            if device.pointer_moved {
                self.events.push(WindowEvent::PointerMoved { position });
            }
            for ButtonChange { button, pressed } in device.buttons.drain(..) {
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

        for (&contact_number, contact) in &mut device.contacts {
            let touch = Touch {
                device: id,
                contact: contact_number,
            };
            let Some(position) = contact.point.position(scale) else {
                continue;
            };
            if self.touch.is_none() && contact.phase == Phase::Down {
                self.touch = Some(touch);
                self.events.push(WindowEvent::PointerMoved { position });
                self.events.push(WindowEvent::PointerPressed {
                    position,
                    button: PointerEventButton::Left,
                });
                contact.moved = false;
            }
            if self.touch != Some(touch) {
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
            shifts_held: 0,
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

impl ScreenPlatform {
    /// Переводит записи `input` в события `window` задачей executor. Замена задачи либо
    /// разрушение платформы отменяют ожидание ввода.
    pub fn deliver_input<T: ComponentHandle + 'static>(
        &mut self,
        executor: &Rc<Executor>,
        input: Session<input::Protocol>,
        window: &T,
    ) {
        let dimension = self.dimension();
        let screen = Screen {
            width: f32::from(u16::try_from(dimension.width).unwrap_or(u16::MAX)),
            height: f32::from(u16::try_from(dimension.height).unwrap_or(u16::MAX)),
            scale: self.scale_factor(),
        };

        let window = window.as_weak();
        let tasks = self.tasks.clone();
        let input = PortChannel::client(input, executor.reactor()).with_wait(Timeout::INFINITE);
        self.input = Some(
            executor
                .spawn_now(async move {
                    deliver(&input, screen, &window, &tasks).await;
                })
                .abort_on_drop(),
        );
    }
}

async fn deliver<T: ComponentHandle + 'static>(
    session: &input::Client<PortChannel<SessionTransport>>,
    screen: Screen,
    window: &slint::Weak<T>,
    tasks: &crate::event_loop::TaskQueue,
) {
    let mut translator = Translator::new(screen);
    loop {
        let records = match session.next().await {
            Ok(records) => records,
            Err(error) => {
                println!("input stopped: {error:?}");
                return;
            }
        };

        for record in records.records() {
            let layout = if record.kind == Kind::DeviceAdded {
                if let Some(device) = record.device {
                    session.describe(device).await.ok()
                } else {
                    None
                }
            } else {
                None
            };

            translator.feed(record, |_| layout);
        }

        let events = translator.take_events();
        if !events.is_empty() {
            let Some(window) = window.upgrade() else {
                return;
            };

            for event in events {
                window.window().dispatch_event(event);
            }

            tasks.notify();
        }
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
        Usage::ENTER | KEYPAD_ENTER => Some(Key::Return),
        Usage::ESCAPE => Some(Key::Escape),
        LEFT_SHIFT => Some(Key::Shift),
        RIGHT_SHIFT => Some(Key::ShiftR),
        Usage::BACKSPACE => Some(Key::Backspace),
        Usage::TAB => Some(Key::Tab),
        Usage::RIGHT => Some(Key::RightArrow),
        Usage::LEFT => Some(Key::LeftArrow),
        Usage::DOWN => Some(Key::DownArrow),
        Usage::UP => Some(Key::UpArrow),
        _ => None,
    }
}

fn typed(usage: Usage, shift: bool) -> Option<char> {
    if usage.page != KEYBOARD_PAGE {
        return None;
    }

    let byte = if let Some(index) = usage.id.checked_sub(KEYPAD_FIRST) {
        KEYPAD.get(usize::from(index))
    } else {
        let table = if shift { TYPED_SHIFTED } else { TYPED };
        table.get(usize::from(usage.id.checked_sub(TYPED_FIRST)?))
    };

    // Нуль в таблице стоит у клавиш без символа.
    byte.filter(|byte| **byte != 0)
        .map(|byte| char::from(*byte))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn text(id: u16, shift: bool) -> Option<char> {
        typed(Usage::new(KEYBOARD_PAGE, id), shift)
    }

    #[test]
    fn printable_keys_follow_the_us_layout_and_shift() {
        assert_eq!(text(0x04, false), Some('a'));
        assert_eq!(text(0x04, true), Some('A'));

        assert_eq!(text(0x1E, false), Some('1'));
        assert_eq!(text(0x27, false), Some('0'));
        assert_eq!(text(0x25, true), Some('*'));

        assert_eq!(text(0x2E, false), Some('='));
        assert_eq!(text(0x2E, true), Some('+'));
        assert_eq!(text(0x38, false), Some('/'));

        assert_eq!(text(0x39, false), None);
        assert_eq!(text(0x28, false), None);
    }

    #[test]
    fn keypad_types_digits_and_operators_regardless_of_shift() {
        assert_eq!(text(0x54, true), Some('/'));
        assert_eq!(text(0x57, false), Some('+'));
        assert_eq!(text(0x58, false), None);

        assert_eq!(text(0x59, false), Some('1'));
        assert_eq!(text(0x62, false), Some('0'));
        assert_eq!(text(0x63, false), Some('.'));

        assert_eq!(text(0x64, false), None);
    }
}
