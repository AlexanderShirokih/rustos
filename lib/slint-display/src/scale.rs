//! Масштаб интерфейса по плотности экрана.

use display_abi::{DisplayDimension, PhysicalSize};

const MILLIMETRES_PER_INCH: f32 = 25.4;

const HANDHELD_DIAGONAL_MAX_MM: f32 = 254.0;
const HANDHELD_REFERENCE_DPI: f32 = 160.0;
const MONITOR_REFERENCE_DPI: f32 = 96.0;

const PLAUSIBLE_DPI_MIN: f32 = 50.0;
const PLAUSIBLE_DPI_MAX: f32 = 1000.0;

/// Число физических пикселей на логический для экрана `dimension` с видимой областью
/// `physical`: плотность экрана, делённая на опорную, но не меньше 1. Опорная плотность -
/// 160 dpi, как dp Android, для экрана с диагональю меньше 254 мм и 96 dpi для остальных.
/// Масштаб равен 1, если размер неизвестен либо плотность лежит вне 50-1000 dpi.
pub fn scale_factor(dimension: DisplayDimension, physical: Option<PhysicalSize>) -> f32 {
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
