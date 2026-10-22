//! Размещение кадра и органов управления в альбомных координатах с общим поворотом.

use alloc::vec::Vec;

use display_abi::{Area, DisplayDimension};

use crate::key::{
    KEY_BACKSPACE, KEY_DOWNARROW, KEY_ENTER, KEY_ESCAPE, KEY_FIRE, KEY_LEFTARROW, KEY_RIGHTARROW,
    KEY_STRAFE_L, KEY_STRAFE_R, KEY_TAB, KEY_UPARROW, KEY_USE,
};

/// Точка в пикселях. Система координат задаётся операцией перевода `Layout`.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct Point {
    pub x: u32,
    pub y: u32,
}

/// Действие площадки. Поворот считается от точки начала касания.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Control {
    Move,
    Turn,
    Fire,
    Weapons,
    Key(u8),
}

/// Область касания и имя действия в альбомных координатах.
#[derive(Debug, Clone, Copy)]
pub struct Button {
    pub area: Area,
    pub control: Control,
    pub round: bool,
    pub label: &'static str,
}

/// Назначение сенсора по состоянию C-движка. Клавиатура не переназначается.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Mode {
    Game,
    Menu,
    MainMenu,
    Confirm,
    Text,
}

/// Неизменное размещение окна. Кадр центрируется с отношением сторон 4:3.
#[derive(Debug, Clone, Copy)]
pub struct Layout {
    rotated: bool,
    frame: Area,
    pub(crate) width: u32,
    pub(crate) height: u32,
}

impl Layout {
    pub fn is_rotated(self) -> bool {
        self.rotated
    }

    /// Область игрового кадра в физических координатах панели.
    pub fn frame(self) -> Area {
        self.frame
    }

    /// Выбирает наибольший кадр 4:3 без поворота или с поворотом на 90° по часовой стрелке.
    /// `None`, если кадр не помещается даже в размере 4×3 пикселя.
    pub fn new(panel: DisplayDimension) -> Option<Self> {
        let upright = (panel.width / 4).min(panel.height / 3);
        let sideways = (panel.width / 3).min(panel.height / 4);
        let unit = upright.max(sideways);
        if unit == 0 {
            return None;
        }
        let rotated = sideways > upright;
        let (width, height) = if rotated {
            (panel.height, panel.width)
        } else {
            (panel.width, panel.height)
        };
        let (fw, fh) = if rotated {
            (3 * unit, 4 * unit)
        } else {
            (4 * unit, 3 * unit)
        };
        Some(Self {
            rotated,
            width,
            height,
            frame: Area {
                x: (panel.width - fw) / 2,
                y: (panel.height - fh) / 2,
                width: fw,
                height: fh,
            },
        })
    }

    /// Переводит точку панели в альбомные координаты, ограничивая её размером панели.
    pub fn logical(self, point: Point) -> Point {
        let panel = self.panel();
        let x = point.x.min(panel.width - 1);
        let y = point.y.min(panel.height - 1);
        if self.rotated {
            Point {
                x: y,
                y: panel.width - 1 - x,
            }
        } else {
            Point { x, y }
        }
    }

    /// Область панели для альбомного прямоугольника, лежащего в окне.
    pub fn physical_area(self, area: Area) -> Area {
        if self.rotated {
            Area {
                x: self.height.saturating_sub(area.y + area.height),
                y: area.x,
                width: area.height,
                height: area.width,
            }
        } else {
            area
        }
    }

    pub fn panel(self) -> DisplayDimension {
        if self.rotated {
            DisplayDimension {
                width: self.height,
                height: self.width,
            }
        } else {
            DisplayDimension {
                width: self.width,
                height: self.height,
            }
        }
    }

    /// Видимые кнопки идут перед невидимой областью поворота для выбора касания.
    pub fn buttons(self, mode: Mode, weapons: bool) -> Vec<Button> {
        let mut buttons = Vec::with_capacity(14);
        let main = (percent(self.width, 14))
            .min(percent(self.height, 32))
            .max(1);
        let small = (self.height / 8).max(1);
        let left = percent(self.width, 17);
        let right = self.width - left;
        let level = percent(self.height, 73);
        let mut add = |x: u32, y: u32, diameter: u32, control, label, round| {
            let diameter = diameter.max(1);
            let x = x.saturating_sub(diameter / 2).min(self.width - 1);
            let y = y.saturating_sub(diameter / 2).min(self.height - 1);
            buttons.push(Button {
                area: Area {
                    x,
                    y,
                    width: diameter.min(self.width - x),
                    height: diameter.min(self.height - y),
                },
                control,
                label,
                round,
            });
        };
        add(left, level, main, Control::Move, "", true);
        let (accept, accept_label, back, back_label) = match mode {
            Mode::Game => (Control::Fire, "FIRE", KEY_USE, "USE"),
            Mode::Menu => (Control::Key(KEY_ENTER), "OK", KEY_BACKSPACE, "BACK"),
            Mode::MainMenu | Mode::Text => (Control::Key(KEY_ENTER), "OK", KEY_ESCAPE, "BACK"),
            Mode::Confirm => (Control::Key(b'y'), "YES", b'n', "NO"),
        };
        let action = if mode == Mode::Game {
            main
        } else {
            main * 4 / 7
        };
        add(right, level, action, accept, accept_label, true);
        add(
            right,
            percent(self.height, 41),
            main * 4 / 7,
            Control::Key(back),
            back_label,
            true,
        );
        if mode == Mode::Game {
            add(
                self.width / 10,
                percent(self.height, 12),
                small,
                Control::Key(KEY_ESCAPE),
                "MENU",
                true,
            );
            add(
                self.width / 10,
                percent(self.height, 30),
                small,
                Control::Key(KEY_TAB),
                "MAP",
                true,
            );
            add(
                self.width - self.width / 10,
                percent(self.height, 12),
                small,
                Control::Weapons,
                "WPN",
                true,
            );
            if weapons {
                for number in 0..7 {
                    add(
                        self.width.saturating_sub(small * 7) / 2 + small * number + small / 2,
                        self.height / 5,
                        small,
                        Control::Key(b'1' + u8::try_from(number).unwrap_or_default()),
                        ["1", "2", "3", "4", "5", "6", "7"]
                            .get(number as usize)
                            .copied()
                            .unwrap_or(""),
                        false,
                    );
                }
            }
        }
        if mode == Mode::Game {
            buttons.push(Button {
                area: Area {
                    x: self.width / 2,
                    y: self.height / 5,
                    width: self.width - self.width / 2,
                    height: self.height - self.height / 5,
                },
                control: Control::Turn,
                label: "",
                round: false,
            });
        }
        buttons
    }
}

impl Button {
    pub fn contains(self, point: Point) -> bool {
        let inside = point.x >= self.area.x
            && point.y >= self.area.y
            && point.x - self.area.x < self.area.width
            && point.y - self.area.y < self.area.height;
        inside
            && (!self.round || {
                let x = i64::from(point.x) - i64::from(self.area.x + self.area.width / 2);
                let y = i64::from(point.y) - i64::from(self.area.y + self.area.height / 2);
                let radius = i64::from(self.area.width.min(self.area.height) / 2);
                x * x + y * y <= radius * radius
            })
    }

    /// Выделение действия по объединённому состоянию клавиш клавиатуры и сенсора.
    pub fn active(self, held: &[bool; 256], mode: Mode) -> bool {
        let keys: &[u8] = match self.control {
            Control::Move if matches!(mode, Mode::Menu | Mode::MainMenu) => {
                &[KEY_UPARROW, KEY_DOWNARROW, KEY_LEFTARROW, KEY_RIGHTARROW]
            }
            Control::Move => &[KEY_UPARROW, KEY_DOWNARROW, KEY_STRAFE_L, KEY_STRAFE_R],
            Control::Turn => &[KEY_LEFTARROW, KEY_RIGHTARROW],
            Control::Fire => &[KEY_FIRE],
            Control::Weapons => return false,
            Control::Key(key) => return held.get(usize::from(key)).copied().unwrap_or(false),
        };
        keys.iter()
            .any(|&key| held.get(usize::from(key)).copied().unwrap_or(false))
    }
}

fn percent(value: u32, ratio: u32) -> u32 {
    value / 100 * ratio + value % 100 * ratio / 100
}
