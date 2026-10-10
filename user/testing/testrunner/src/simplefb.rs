//! Геометрия и перенос пикселей `driver-simplefb` на обычных буферах.

use alloc::vec::Vec;
use core::cell::{Cell, RefCell};

use display_abi::{Area, DisplayDimension, DisplayError, PhysicalSize, SURFACE_PIXEL_BYTES};
use driver_simplefb::{Framebuffer, Refusal, SIMPLE_FRAMEBUFFER_COMPATIBLE};
use fdt_test::Token;
use platform_abi::{Description, DeviceId};

const UNTOUCHED_SCREEN_BYTE: u8 = 0xAA;

const A8R8G8B8_PIXEL_BYTES: usize = 4;
const R8G8B8_PIXEL_BYTES: usize = 3;

const SURFACE_FULL_RED_HALF_BLUE_TOP_5A: u32 = 0x5AFF_0080;

const SIZED_TREE: &[Token<'static>] = &fdt_test::tree! {
    sized@f000 {
        compatible = "simple-framebuffer";
        width = <4>;
        height = <2>;
        stride = <16>;
        format = "a8r8g8b8";
        width-mm = <68>;
        height-mm = <147>;
    };
    zero-width-mm@c000 {
        compatible = "simple-framebuffer";
        width = <4>;
        height = <2>;
        stride = <16>;
        format = "a8r8g8b8";
        width-mm = <0>;
        height-mm = <147>;
    };
    height-mm-only@d000 {
        compatible = "simple-framebuffer";
        width = <4>;
        height = <2>;
        stride = <16>;
        format = "a8r8g8b8";
        height-mm = <147>;
    };
    wide-width-mm@e000 {
        compatible = "simple-framebuffer";
        width = <4>;
        height = <2>;
        stride = <16>;
        format = "a8r8g8b8";
        width-mm = <0 68>;
        height-mm = <147>;
    };
};

const TREE: &[Token<'static>] = &fdt_test::tree! {
    phone@1000 {
        compatible = "simple-framebuffer";
        width = <1080>;
        height = <2340>;
        stride = <4320>;
        format = "a8r8g8b8";
    };
    padded@2000 {
        compatible = "simple-framebuffer";
        width = <70>;
        height = <3>;
        stride = <288>;
        format = "a8r8g8b8";
    };
    short-stride@3000 {
        compatible = "simple-framebuffer";
        width = <70>;
        height = <3>;
        stride = <276>;
        format = "a8r8g8b8";
    };
    empty@4000 {
        compatible = "simple-framebuffer";
        width = <0>;
        height = <3>;
        stride = <288>;
        format = "a8r8g8b8";
    };
    reserved@5000 {
        compatible = "simple-framebuffer";
        memory-region = <1>;
        width = <70>;
        height = <3>;
        stride = <288>;
        format = "a8r8g8b8";
    };
    unknown-format@6000 {
        compatible = "simple-framebuffer";
        width = <70>;
        height = <3>;
        stride = <288>;
        format = "c8";
    };
    unsized@7000 {
        compatible = "simple-framebuffer";
        width = <70>;
        height = <3>;
        format = "a8r8g8b8";
    };
    serial@8000 {
        compatible = "arm,pl011";
    };
    unpadded@9000 {
        compatible = "simple-framebuffer";
        width = <4>;
        height = <2>;
        stride = <16>;
        format = "a8r8g8b8";
    };
    packed@a000 {
        compatible = "simple-framebuffer";
        width = <4>;
        height = <2>;
        stride = <16>;
        format = "r8g8b8";
    };
    packed-short-stride@b000 {
        compatible = "simple-framebuffer";
        width = <4>;
        height = <2>;
        stride = <11>;
        format = "r8g8b8";
    };
};

fn framebuffer(path: &str) -> Result<Framebuffer, Refusal> {
    framebuffer_of(TREE, path)
}

fn framebuffer_of(tree: &[Token<'_>], path: &str) -> Result<Framebuffer, Refusal> {
    let blob = fdt_test::build(tree);
    let description = Description::new(&blob, 0).expect("tree parses");
    let device = DeviceId::new(path).expect("absolute path");
    Framebuffer::open(&description, &device)
}

fn single(format: &str, width: u32, height: u32, stride: u32) -> Framebuffer {
    let blob = fdt_test::build(&[
        Token::Node("single@0"),
        Token::Strings("compatible", &[SIMPLE_FRAMEBUFFER_COMPATIBLE]),
        Token::Cells("width", &[width]),
        Token::Cells("height", &[height]),
        Token::Cells("stride", &[stride]),
        Token::Strings("format", &[format]),
        Token::End,
    ]);
    let description = Description::new(&blob, 0).expect("tree parses");
    let device = DeviceId::new("/single@0").expect("absolute path");
    Framebuffer::open(&description, &device).expect("format accepted")
}

fn written(framebuffer: &Framebuffer, frame_size: usize, area: Area) -> Vec<usize> {
    let surface =
        alloc::vec![0; framebuffer.dimension().surface_size().expect("surface fits") as usize];
    let mut screen = alloc::vec![UNTOUCHED_SCREEN_BYTE; frame_size];
    present(framebuffer, area, &surface, &mut screen).expect("area inside the screen");
    (0..frame_size)
        .filter(|&offset| screen[offset] != UNTOUCHED_SCREEN_BYTE)
        .collect()
}

fn present(
    framebuffer: &Framebuffer,
    area: Area,
    surface: &[u8],
    screen: &mut [u8],
) -> Result<(), DisplayError> {
    let screen = RefCell::new(screen);
    let write = |offset: usize, pixels: &[u8]| {
        let mut screen = screen.borrow_mut();
        let target = screen.get_mut(offset..offset + pixels.len()).ok_or(())?;
        target.copy_from_slice(pixels);
        Ok(())
    };
    framebuffer.present::<()>(
        area,
        |offset, pixels| {
            let source = surface.get(offset..offset + pixels.len()).ok_or(())?;
            pixels.copy_from_slice(source);
            Ok(())
        },
        write,
        |surface_offset, screen_offset, len| {
            let source = surface
                .get(surface_offset..surface_offset + len)
                .ok_or(())?;
            write(screen_offset, source)
        },
    )
}

#[test_harness::test]
fn a_described_framebuffer_reports_its_dimension_and_needs_a_window_of_its_frame() {
    let phone = framebuffer("/phone@1000").expect("framebuffer accepted");
    let frame = 4320 * 2340;

    test_harness::check_eq!(
        phone.dimension(),
        DisplayDimension {
            width: 1080,
            height: 2340
        }
    );
    test_harness::check_eq!(phone.stride(), 4320);
    test_harness::check_eq!(phone.format(), "a8r8g8b8");
    test_harness::check_eq!(phone.fits(frame), Ok(()));
    test_harness::check_eq!(phone.fits(frame - 1), Err(Refusal::Malformed));
}

#[test_harness::test]
fn a_physical_size_needs_both_nonzero_millimetre_properties() {
    let sized = framebuffer_of(SIZED_TREE, "/sized@f000").expect("framebuffer accepted");
    let unsized_framebuffer = framebuffer("/unpadded@9000").expect("framebuffer accepted");

    test_harness::check_eq!(
        sized.physical_size(),
        Some(PhysicalSize {
            width_mm: 68,
            height_mm: 147,
        })
    );
    test_harness::check_eq!(unsized_framebuffer.physical_size(), None);
    for path in ["/zero-width-mm@c000", "/height-mm-only@d000"] {
        let framebuffer = framebuffer_of(SIZED_TREE, path).expect("framebuffer accepted");
        test_harness::check_eq!(framebuffer.physical_size(), None);
    }
    test_harness::check_eq!(
        framebuffer_of(SIZED_TREE, "/wide-width-mm@e000"),
        Err(Refusal::Malformed)
    );
}

#[test_harness::test]
fn a_framebuffer_description_outside_the_supported_one_is_refused() {
    let refused = [
        ("/short-stride@3000", Refusal::Malformed),
        ("/empty@4000", Refusal::Malformed),
        ("/reserved@5000", Refusal::Unsupported),
        ("/unknown-format@6000", Refusal::Unsupported),
        ("/unsized@7000", Refusal::Malformed),
        ("/packed-short-stride@b000", Refusal::Malformed),
        ("/serial@8000", Refusal::Unsupported),
        ("/absent@0", Refusal::NotDescribed),
    ];

    for (path, refusal) in refused {
        test_harness::check_eq!(framebuffer(path), Err(refusal));
    }
}

#[test_harness::test]
fn each_device_of_one_description_presents_with_its_own_geometry() {
    let padded = framebuffer("/padded@2000").expect("framebuffer accepted");
    let unpadded = framebuffer("/unpadded@9000").expect("framebuffer accepted");
    let packed = framebuffer("/packed@a000").expect("framebuffer accepted");
    let area = Area {
        x: 1,
        y: 1,
        width: 2,
        height: 1,
    };

    test_harness::check_eq!(
        padded.dimension(),
        DisplayDimension {
            width: 70,
            height: 3
        }
    );
    test_harness::check_eq!(
        unpadded.dimension(),
        DisplayDimension {
            width: 4,
            height: 2
        }
    );
    test_harness::check_eq!(
        written(&padded, 288 * 3, area),
        (288 + A8R8G8B8_PIXEL_BYTES..288 + 3 * A8R8G8B8_PIXEL_BYTES).collect::<Vec<_>>()
    );
    test_harness::check_eq!(
        written(&unpadded, 16 * 2, area),
        (16 + A8R8G8B8_PIXEL_BYTES..16 + 3 * A8R8G8B8_PIXEL_BYTES).collect::<Vec<_>>()
    );
    test_harness::check_eq!(
        written(&packed, 16 * 2, area),
        (16 + R8G8B8_PIXEL_BYTES..16 + 3 * R8G8B8_PIXEL_BYTES).collect::<Vec<_>>()
    );
}

#[test_harness::test]
fn each_binding_format_encodes_a_surface_pixel_and_a_surface_layout_keeps_its_top_byte() {
    let encoded: [(&str, &[u8]); 11] = [
        ("r5g6b5", &[0x10, 0xF8]),
        ("r5g5b5a1", &[0x21, 0xF8]),
        ("x1r5g5b5", &[0x10, 0x7C]),
        ("a1r5g5b5", &[0x10, 0xFC]),
        ("r8g8b8", &[0x80, 0x00, 0xFF]),
        ("x8r8g8b8", &[0x80, 0x00, 0xFF, 0x5A]),
        ("a8r8g8b8", &[0x80, 0x00, 0xFF, 0x5A]),
        ("x8b8g8r8", &[0xFF, 0x00, 0x80, 0x00]),
        ("a8b8g8r8", &[0xFF, 0x00, 0x80, 0xFF]),
        ("x2r10g10b10", &[0x02, 0x02, 0xF0, 0x3F]),
        ("a2r10g10b10", &[0x02, 0x02, 0xF0, 0xFF]),
    ];
    let pixel = Area {
        x: 0,
        y: 0,
        width: 1,
        height: 1,
    };

    for (format, expected) in encoded {
        let framebuffer = single(format, 1, 1, expected.len() as u32);
        let mut screen = alloc::vec![UNTOUCHED_SCREEN_BYTE; expected.len()];

        let presented = present(
            &framebuffer,
            pixel,
            &SURFACE_FULL_RED_HALF_BLUE_TOP_5A.to_le_bytes(),
            &mut screen,
        );

        test_harness::check_eq!(presented, Ok(()));
        test_harness::check_eq!((format, screen.as_slice()), (format, expected));
    }
}

#[test_harness::test]
fn present_writes_the_area_and_keeps_the_neighbours_and_the_row_padding() {
    let surface: Vec<u8> = (0..70 * 3 * SURFACE_PIXEL_BYTES as usize)
        .map(|byte| byte as u8)
        .collect();
    // Ширина больше порции кодирования в 64 пикселя.
    let area = Area {
        x: 1,
        y: 1,
        width: 66,
        height: 1,
    };
    // Байт пикселя экрана - номер байта пикселя поверхности (B, G, R, старший) либо ноль.
    let layouts = [
        ("a8r8g8b8", [Some(0), Some(1), Some(2), Some(3)]),
        ("x8b8g8r8", [Some(2), Some(1), Some(0), None]),
    ];

    for (format, sources) in layouts {
        let framebuffer = single(format, 70, 3, 288);
        let mut screen = alloc::vec![UNTOUCHED_SCREEN_BYTE; 288 * 3];

        let presented = present(&framebuffer, area, &surface, &mut screen);

        test_harness::check_eq!(presented, Ok(()));
        for (offset, &byte) in screen.iter().enumerate() {
            let (row, column) = (offset / 288, offset % 288);
            let inside =
                row == 1 && (A8R8G8B8_PIXEL_BYTES..67 * A8R8G8B8_PIXEL_BYTES).contains(&column);
            let pixel_start = 70 * SURFACE_PIXEL_BYTES as usize + column / 4 * 4;
            let expected = match (inside, sources[column % 4]) {
                (false, _) => UNTOUCHED_SCREEN_BYTE,
                (true, Some(source)) => surface[pixel_start + source],
                (true, None) => 0,
            };
            test_harness::check_eq!((format, offset, byte), (format, offset, expected));
        }
    }
}

#[test_harness::test]
fn present_of_an_area_past_the_screen_is_out_of_bounds_before_any_copy() {
    let padded = framebuffer("/padded@2000").expect("framebuffer accepted");
    let past_edge = Area {
        x: 66,
        y: 0,
        width: 8,
        height: 1,
    };

    let copies = Cell::new(0);
    let presented = padded.present(
        past_edge,
        |_, _| {
            copies.set(copies.get() + 1);
            Ok::<_, ()>(())
        },
        |_, _| {
            copies.set(copies.get() + 1);
            Ok(())
        },
        |_, _, _| {
            copies.set(copies.get() + 1);
            Ok(())
        },
    );

    test_harness::check_eq!(presented, Err(DisplayError::OutOfBounds));
    test_harness::check_eq!(copies.get(), 0);
}
