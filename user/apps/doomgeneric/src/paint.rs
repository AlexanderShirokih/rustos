//! Значки и выделение кнопок в пикселях поверхности BGRA.

use doomgeneric::{Button, Control, Point};

const MENU_ICON: [u32; 24] = [
    0x0000_0000,
    0x0000_0000,
    0x0000_0000,
    0x0000_0000,
    0x0000_0000,
    0x0001_c1c0,
    0x0001_c1c0,
    0x0001_c1c0,
    0x0001_c1c0,
    0x0001_c1c0,
    0x0001_c1c0,
    0x0001_c1c0,
    0x0001_c1c0,
    0x0001_c1c0,
    0x0001_c1c0,
    0x0001_c1c0,
    0x0001_c1c0,
    0x0001_c1c0,
    0x0001_c1c0,
    0x0001_c1c0,
    0x0000_0000,
    0x0000_0000,
    0x0000_0000,
    0x0000_0000,
];

const FIRE_ICON: [u32; 24] = [
    0x0000_0000,
    0x0000_0000,
    0x0000_0800,
    0x0000_0800,
    0x0000_0800,
    0x0000_7f00,
    0x0001_ebc0,
    0x0003_80e0,
    0x0003_0060,
    0x0006_0030,
    0x0006_0030,
    0x0004_0010,
    0x003e_003e,
    0x0004_0010,
    0x0006_0030,
    0x0006_0030,
    0x0003_0060,
    0x0003_80e0,
    0x0001_ebc0,
    0x0000_7f00,
    0x0000_0800,
    0x0000_0800,
    0x0000_0800,
    0x0000_0000,
];

const USE_ICON: [u32; 24] = [
    0x0000_0000,
    0x0000_0700,
    0x0000_0d80,
    0x0000_7880,
    0x0000_d8f0,
    0x0000_88d8,
    0x0000_8888,
    0x0000_8888,
    0x0000_888e,
    0x0000_888f,
    0x0000_8889,
    0x001e_8009,
    0x0036_8001,
    0x0033_8001,
    0x0031_8001,
    0x0019_8001,
    0x000c_0001,
    0x000c_0003,
    0x0006_0003,
    0x0003_0006,
    0x0003_8006,
    0x0001_c00c,
    0x0000_e03c,
    0x0000_7ff0,
];

const BACK_ICON: [u32; 24] = [
    0x0000_0000,
    0x0000_0000,
    0x0000_0000,
    0x0000_0000,
    0x0000_0000,
    0x0000_6000,
    0x0000_e000,
    0x0001_c000,
    0x0003_8000,
    0x0007_0000,
    0x000e_0000,
    0x001c_0000,
    0x001f_fffc,
    0x001c_0000,
    0x000e_0000,
    0x0007_0000,
    0x0003_8000,
    0x0001_c000,
    0x0000_e000,
    0x0000_6000,
    0x0000_0000,
    0x0000_0000,
    0x0000_0000,
    0x0000_0000,
];

const OK_ICON: [u32; 24] = [
    0x0000_0000,
    0x0000_0000,
    0x0000_0000,
    0x0000_0000,
    0x0000_0000,
    0x0000_0000,
    0x0000_0018,
    0x0000_0038,
    0x0000_0030,
    0x0000_0060,
    0x0000_00c0,
    0x0000_0180,
    0x0000_0300,
    0x000c_0600,
    0x000e_0c00,
    0x0007_1800,
    0x0003_b000,
    0x0001_f000,
    0x0000_e000,
    0x0000_0000,
    0x0000_0000,
    0x0000_0000,
    0x0000_0000,
    0x0000_0000,
];

const NO_ICON: [u32; 24] = [
    0x0000_0000,
    0x0000_0000,
    0x0000_0000,
    0x0000_0000,
    0x0000_0000,
    0x0000_0000,
    0x0003_0060,
    0x0003_80e0,
    0x0001_c1c0,
    0x0000_e380,
    0x0000_7700,
    0x0000_3e00,
    0x0000_1c00,
    0x0000_3e00,
    0x0000_7700,
    0x0000_e380,
    0x0001_c1c0,
    0x0003_80e0,
    0x0003_0060,
    0x0000_0000,
    0x0000_0000,
    0x0000_0000,
    0x0000_0000,
    0x0000_0000,
];

const MAP_ICON: [u32; 24] = [
    0x0000_0000,
    0x0000_0000,
    0x0000_0000,
    0x0001_f01c,
    0x000f_fefc,
    0x001c_47c4,
    0x0010_4104,
    0x0010_4104,
    0x0010_4104,
    0x0010_4104,
    0x0010_4104,
    0x0010_4104,
    0x0010_4104,
    0x0010_4104,
    0x0010_4104,
    0x0010_4104,
    0x0010_4104,
    0x0010_4104,
    0x0010_4104,
    0x0011_f11c,
    0x001f_bff8,
    0x001c_07c0,
    0x0000_0000,
    0x0000_0000,
];

const WPN_ICON: [u32; 24] = [
    0x0000_0000,
    0x0000_0000,
    0x0000_1c00,
    0x0000_7700,
    0x0001_e3c0,
    0x0007_80f0,
    0x000e_0038,
    0x0018_000c,
    0x000e_0038,
    0x0007_80f0,
    0x0001_e3c0,
    0x0000_7700,
    0x0018_1c0c,
    0x000e_0038,
    0x0007_80f0,
    0x0001_e3c0,
    0x0000_7700,
    0x0018_1c0c,
    0x000e_0038,
    0x0007_80f0,
    0x0001_e3c0,
    0x0000_7700,
    0x0000_1c00,
    0x0000_0000,
];

pub(crate) fn pixel(button: Button, point: Point, active: bool, knob: Point) -> Option<u32> {
    if !button.contains(point) {
        return None;
    }
    let x = point.x.saturating_sub(button.area.x);
    let y = point.y.saturating_sub(button.area.y);
    let width = button.area.width;
    let height = button.area.height;
    let border = (width.min(height) / 100).max(1);
    let distance = |a: Point, b: Point| {
        let dx = i64::from(a.x) - i64::from(b.x);
        let dy = i64::from(a.y) - i64::from(b.y);
        dx * dx + dy * dy
    };
    if button.control == Control::Move && distance(point, knob) <= i64::from(width / 5).pow(2) {
        return Some(0xff89_969b);
    }
    let center = Point {
        x: button.area.x + width / 2,
        y: button.area.y + height / 2,
    };
    let outline = if button.round {
        distance(point, center) >= i64::from((width.min(height) / 2).saturating_sub(border)).pow(2)
    } else {
        x < border || y < border || x + border >= width || y + border >= height
    };
    if outline {
        return Some(0xff8b_a5b5);
    }
    if let Some(bitmap) = icon(button.label) {
        let scale = (width.min(height) * 3 / 7 / 24).max(1);
        let left = width.saturating_sub(24 * scale) / 2;
        let top = height.saturating_sub(24 * scale) / 2;
        if x >= left && y >= top {
            let column = (x - left) / scale;
            let row = (y - top) / scale;
            if column < 24
                && usize::try_from(row)
                    .ok()
                    .and_then(|i| bitmap.get(i))
                    .is_some_and(|bits| bits & (1 << (23 - column)) != 0)
            {
                return Some(0xfff4_f1e5);
            }
        }
    } else if let Some(&number) = button.label.as_bytes().first() {
        let scale = (width / 10).max(1);
        if mark(number, x, y, width / 2, height / 2, scale) {
            return Some(0xfff4_f1e5);
        }
    } else if button.control == Control::Move {
        let scale = (width / 70).max(1);
        for (arrow, cx, cy) in [
            (b'^', width / 2, height / 8),
            (b'v', width / 2, height * 7 / 8),
            (b'<', width / 8, height / 2),
            (b'>', width * 7 / 8, height / 2),
        ] {
            if mark(arrow, x, y, cx, cy, scale) {
                return Some(0xff8b_a5b5);
            }
        }
    }
    Some(if active { 0xff32_758b } else { 0xff18_232d })
}

pub(crate) fn knob(button: Button, touch: Option<Point>) -> Point {
    let center = Point {
        x: button.area.x + button.area.width / 2,
        y: button.area.y + button.area.height / 2,
    };
    let Some(touch) = touch else {
        return center;
    };
    let mut dx = i64::from(touch.x) - i64::from(center.x);
    let mut dy = i64::from(touch.y) - i64::from(center.y);
    let length = u64::try_from(
        (i128::from(dx).pow(2) + i128::from(dy).pow(2))
            .unsigned_abs()
            .isqrt(),
    )
    .unwrap_or(u64::MAX);
    let radius = i64::from(button.area.width / 4);
    if length > u64::try_from(radius).unwrap_or_default() {
        let length = i64::try_from(length).unwrap_or(i64::MAX);
        dx = dx * radius / length;
        dy = dy * radius / length;
    }
    Point {
        x: u32::try_from(i64::from(center.x) + dx).unwrap_or(center.x),
        y: u32::try_from(i64::from(center.y) + dy).unwrap_or(center.y),
    }
}

fn mark(letter: u8, x: u32, y: u32, cx: u32, cy: u32, scale: u32) -> bool {
    let left = cx.saturating_sub(5 * scale / 2);
    let top = cy.saturating_sub(7 * scale / 2);
    if x < left || y < top {
        return false;
    }
    let column = (x - left) / scale;
    let row = (y - top) / scale;
    column < 5
        && usize::try_from(row)
            .ok()
            .and_then(|i| glyph(letter).get(i).copied())
            .is_some_and(|bits| bits & (16 >> column) != 0)
}

fn glyph(letter: u8) -> [u8; 7] {
    match letter {
        b'^' => [4, 10, 17, 0, 0, 0, 0],
        b'v' => [0, 0, 0, 0, 17, 10, 4],
        b'<' => [0, 2, 4, 8, 4, 2, 0],
        b'>' => [0, 8, 4, 2, 4, 8, 0],
        b'1' => [4, 12, 4, 4, 4, 4, 14],
        b'2' => [14, 17, 1, 2, 4, 8, 31],
        b'3' => [30, 1, 1, 14, 1, 1, 30],
        b'4' => [2, 6, 10, 18, 31, 2, 2],
        b'5' => [31, 16, 16, 30, 1, 1, 30],
        b'6' => [14, 16, 16, 30, 17, 17, 14],
        b'7' => [31, 1, 2, 4, 8, 8, 8],
        _ => [0; 7],
    }
}

fn icon(label: &str) -> Option<&'static [u32; 24]> {
    match label {
        "MENU" => Some(&MENU_ICON),
        "FIRE" => Some(&FIRE_ICON),
        "USE" => Some(&USE_ICON),
        "BACK" => Some(&BACK_ICON),
        "OK" | "YES" => Some(&OK_ICON),
        "NO" => Some(&NO_ICON),
        "MAP" => Some(&MAP_ICON),
        "WPN" => Some(&WPN_ICON),
        _ => None,
    }
}
