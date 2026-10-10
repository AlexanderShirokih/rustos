use display_abi::{DisplayDimension, PhysicalSize};
use slint_display::scale_factor;

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
