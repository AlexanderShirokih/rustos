//! Размер экрана и прямоугольник на экране.

use crate::error::DisplayError;

/// Размер пикселя поверхности в байтах.
pub const SURFACE_PIXEL_BYTES: u32 = 4;

ipc::wire_value! {
    /// Размер экрана в пикселях.
    #[derive(Debug, Clone, Copy, PartialEq, Eq)]
    pub struct DisplayDimension {
        pub width: u32,
        pub height: u32,
    }
}

ipc::wire_value! {
    /// Прямоугольник экрана в пикселях, заданный левым верхним углом и размером.
    #[derive(Debug, Clone, Copy, PartialEq, Eq)]
    pub struct Area {
        pub x: u32,
        pub y: u32,
        pub width: u32,
        pub height: u32,
    }
}

impl DisplayDimension {
    /// Шаг строки поверхности в байтах.
    pub fn surface_stride(self) -> u64 {
        u64::from(self.width) * u64::from(SURFACE_PIXEL_BYTES)
    }

    /// Размер поверхности в байтах, равный шагу строки, умноженному на высоту экрана. `None`,
    /// если размер не помещается в `u64`.
    pub fn surface_size(self) -> Option<u64> {
        self.surface_stride().checked_mul(u64::from(self.height))
    }

    /// Возвращает `OutOfBounds`, если у `area` нулевая ширина либо высота или `area` выходит за
    /// экран.
    pub fn check_area(self, area: Area) -> Result<(), DisplayError> {
        let inside = |start: u32, len: u32, limit: u32| {
            len != 0 && u64::from(start) + u64::from(len) <= u64::from(limit)
        };

        if inside(area.x, area.width, self.width) && inside(area.y, area.height, self.height) {
            Ok(())
        } else {
            Err(DisplayError::OutOfBounds)
        }
    }
}

#[cfg(test)]
mod tests {
    use super::{Area, DisplayDimension, DisplayError};

    const PHONE: DisplayDimension = DisplayDimension {
        width: 1080,
        height: 2340,
    };

    fn area(x: u32, y: u32, width: u32, height: u32) -> Area {
        Area {
            x,
            y,
            width,
            height,
        }
    }

    #[test]
    fn surface_of_a_1080_by_2340_screen_holds_4320_byte_rows() {
        assert_eq!(PHONE.surface_stride(), 4320);
        assert_eq!(PHONE.surface_size(), Some(4320 * 2340));
    }

    #[test]
    fn surface_size_past_u64_is_none() {
        let widest = DisplayDimension {
            width: u32::MAX,
            height: u32::MAX,
        };

        assert_eq!(widest.surface_size(), None);
    }

    #[test]
    fn an_area_within_the_screen_is_accepted() {
        assert_eq!(PHONE.check_area(area(10, 20, 100, 200)), Ok(()));
        assert_eq!(PHONE.check_area(area(0, 0, 1080, 2340)), Ok(()));
    }

    #[test]
    fn an_empty_area_or_one_past_the_screen_edge_is_out_of_bounds() {
        let refused = [
            area(1000, 0, 100, 1),
            area(0, 2300, 1, 41),
            area(10, 10, 0, 5),
            area(10, 10, 5, 0),
            area(u32::MAX, 0, 2, 1),
            area(0, u32::MAX, 1, 2),
        ];

        for area in refused {
            assert_eq!(
                PHONE.check_area(area),
                Err(DisplayError::OutOfBounds),
                "{area:?}"
            );
        }
    }
}
