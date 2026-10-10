//! Пиксель поверхности `Display` для программного рендерера Slint.

use slint::platform::software_renderer::{PremultipliedRgbaColor, TargetPixel};

#[repr(transparent)]
#[derive(Clone, Copy)]
pub(crate) struct Xrgb(u32);

impl TargetPixel for Xrgb {
    fn blend(&mut self, color: PremultipliedRgbaColor) {
        self.0 = blend(self.0, keep(color), rgb(color));
    }

    fn blend_slice(slice: &mut [Self], color: PremultipliedRgbaColor) {
        let (keep, add) = (keep(color), rgb(color));
        if keep == 0 {
            slice.fill(Self(add));
            return;
        }
        // Тело цикла без ветвлений, и компилятор смешивает несколько пикселей за итерацию.
        for pixel in slice {
            pixel.0 = blend(pixel.0, keep, add);
        }
    }

    fn from_rgb(red: u8, green: u8, blue: u8) -> Self {
        Self(u32::from(red) << 16 | u32::from(green) << 8 | u32::from(blue))
    }
}

fn keep(color: PremultipliedRgbaColor) -> u32 {
    u32::from(u8::MAX - color.alpha)
}

fn rgb(color: PremultipliedRgbaColor) -> u32 {
    Xrgb::from_rgb(color.red, color.green, color.blue).0
}

fn blend(pixel: u32, keep: u32, add: u32) -> u32 {
    // R и B умножаются одним умножением в полях по 16 бит, G - отдельно. Для `t < 2^16`
    // частное `t / 255` равно `(t + 1 + (t >> 8)) >> 8`. Канал premultiplied-цвета не
    // больше его альфы, и сумма канала с `add` не переносится в соседний канал.
    let rb = (pixel & 0x00ff_00ff) * keep;
    let rb = ((rb + 0x0001_0001 + ((rb >> 8) & 0x00ff_00ff)) >> 8) & 0x00ff_00ff;
    let g = ((pixel >> 8) & 0xff) * keep;
    let g = ((g + 1 + (g >> 8)) >> 8) << 8;
    (rb | g) + add
}

#[cfg(test)]
mod tests {
    use super::*;

    fn color(alpha: u8) -> PremultipliedRgbaColor {
        PremultipliedRgbaColor {
            red: alpha,
            green: alpha / 2,
            blue: alpha / 3,
            alpha,
        }
    }

    fn pixel(value: u8) -> Xrgb {
        Xrgb::from_rgb(value, u8::MAX - value, value / 2)
    }

    fn slint_rgb8_blend(pixel: Xrgb, color: PremultipliedRgbaColor) -> u32 {
        let keep = u32::from(u8::MAX - color.alpha);
        let channel =
            |shift: u32, add: u8| ((pixel.0 >> shift) & 0xff) * keep / 255 + u32::from(add);
        channel(16, color.red) << 16 | channel(8, color.green) << 8 | channel(0, color.blue)
    }

    #[test]
    fn blend_matches_the_channel_formula_of_slint() {
        for alpha in 0..=u8::MAX {
            for value in 0..=u8::MAX {
                let mut blended = pixel(value);
                blended.blend(color(alpha));

                assert_eq!(blended.0, slint_rgb8_blend(pixel(value), color(alpha)));
            }
        }
    }

    #[test]
    fn a_slice_blends_as_its_pixels_do() {
        for alpha in 0..=u8::MAX {
            let mut slice: Vec<Xrgb> = (0..=u8::MAX).map(pixel).collect();

            Xrgb::blend_slice(&mut slice, color(alpha));

            let blended: Vec<u32> = slice.iter().map(|pixel| pixel.0).collect();
            let expected: Vec<u32> = (0..=u8::MAX)
                .map(|value| slint_rgb8_blend(pixel(value), color(alpha)))
                .collect();
            assert_eq!(blended, expected);
        }
    }
}
