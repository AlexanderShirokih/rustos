//! Масштаб интерфейса по плотности экрана.
//!
//! Масштаб равен плотности экрана, делённой на опорную, и не меньше 1. Опорная плотность -
//! 160 dpi для экрана с диагональю меньше 254 мм, как dp Android, и 96 dpi для остальных. Без
//! физического размера либо при плотности вне 50-1000 dpi масштаб равен 1.

use display_abi::{DisplayDimension, PhysicalSize};

const MILLIMETRES_PER_INCH: f32 = 25.4;

const HANDHELD_DIAGONAL_MAX_MM: f32 = 254.0;
const HANDHELD_REFERENCE_DPI: f32 = 160.0;
const MONITOR_REFERENCE_DPI: f32 = 96.0;

const PLAUSIBLE_DPI_MIN: f32 = 50.0;
const PLAUSIBLE_DPI_MAX: f32 = 1000.0;

pub(crate) fn scale_factor(dimension: DisplayDimension, physical: Option<PhysicalSize>) -> f32 {
    let Some(physical) = physical else {
        return 1.0;
    };
    let length = |side: u32| f32::from(u16::try_from(side).unwrap_or(u16::MAX));

    let diagonal_pixels = length(dimension.width).hypot(length(dimension.height));
    let diagonal_mm = length(physical.width_mm).hypot(length(physical.height_mm));
    let dpi = diagonal_pixels / diagonal_mm * MILLIMETRES_PER_INCH;
    if !(PLAUSIBLE_DPI_MIN..=PLAUSIBLE_DPI_MAX).contains(&dpi) {
        return 1.0;
    }

    let reference = if diagonal_mm < HANDHELD_DIAGONAL_MAX_MM {
        HANDHELD_REFERENCE_DPI
    } else {
        MONITOR_REFERENCE_DPI
    };
    (dpi / reference).max(1.0)
}

#[cfg(test)]
mod tests {
    use display_abi::{DisplayDimension, PhysicalSize};

    use super::scale_factor;

    fn scale(width: u32, height: u32, physical: Option<(u32, u32)>) -> f32 {
        scale_factor(
            DisplayDimension { width, height },
            physical.map(|(width_mm, height_mm)| PhysicalSize {
                width_mm,
                height_mm,
            }),
        )
    }

    #[test]
    fn the_scale_divides_the_density_by_the_reference_of_the_screen_class() {
        let phone = scale(1080, 2340, Some((68, 147)));
        let monitor = scale(1920, 1080, Some((531, 299)));
        let dense_monitor = scale(3840, 2160, Some((597, 336)));

        assert!((phone - 2.53).abs() < 0.01, "{phone}");
        assert!((monitor - 1.0).abs() < f32::EPSILON, "{monitor}");
        assert!((dense_monitor - 1.70).abs() < 0.01, "{dense_monitor}");
    }

    #[test]
    fn an_unknown_or_implausible_size_keeps_scale_one() {
        assert!((scale(1080, 2340, None) - 1.0).abs() < f32::EPSILON);
        assert!((scale(1920, 1080, Some((16, 9))) - 1.0).abs() < f32::EPSILON);
    }
}
