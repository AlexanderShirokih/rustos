//! Геометрия и перенос пикселей `driver-simplefb` на обычных буферах и вывод на описанные
//! платформой буферы кадра через экспорт `Display` на каждый экран.

use alloc::vec::Vec;

use component_abi::RequirementDeclaration;
use component_startup::Found;
use display_abi::{Area, DisplayDimension, DisplayError, SURFACE_PIXEL_BYTES, display};
use driver_simplefb::{Framebuffer, Refusal, SIMPLE_FRAMEBUFFER_COMPATIBLE};
use fdt_test::Token;
use ipc::Protocol;
use kernel_objects::{Mapping, MemoryRegion, SessionTransport};
use kernel_tests::kernel_test;
use platform_abi::{Description, DeviceId};
use runtime::{Timeout, UserMemFlags};
use userland::USERLAND_PAGE_SIZE;

use crate::{
    composition::Importer,
    rootkeeper_system::{finish, with_drivers},
};

const DISPLAY_REQUIREMENT: &str = "displays";

const SIMPLEFB_REPLY_TIMEOUT: Timeout = Timeout::from_secs(2);

const UNTOUCHED_SCREEN_BYTE: u8 = 0xAA;

const OPAQUE_ALPHA: u8 = 0xFF;

const A8R8G8B8_PIXEL_BYTES: usize = 4;
const R8G8B8_PIXEL_BYTES: usize = 3;

const SURFACE_FULL_RED_HALF_BLUE: u32 = 0x00FF_0080;

const SURFACE_CHANNEL_MAX: u64 = 0xFF;
const SURFACE_RED_SHIFT: u32 = 16;
const SURFACE_GREEN_SHIFT: u32 = 8;

const RECTANGLE_COLOR: u32 = 0x00FF_00FF;

const SIMPLEFB_MEMORY: u64 = 16 * 1024 * 1024;

const DISPLAY_IMPORTS: &[crate::composition::Import] = &[crate::composition::Import {
    declaration: RequirementDeclaration::all(DISPLAY_REQUIREMENT, display::Protocol::ID),
    namespace_root: None,
}];

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
    let blob = fdt_test::build(TREE);
    let description = Description::new(&blob, 0).expect("tree parses");
    let device = DeviceId::new(path).expect("absolute path");
    Framebuffer::open(&description, &device)
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
    framebuffer.present::<()>(
        area,
        |offset, pixels| {
            let source = surface.get(offset..offset + pixels.len()).ok_or(())?;
            pixels.copy_from_slice(source);
            Ok(())
        },
        |offset, pixels| {
            let target = screen.get_mut(offset..offset + pixels.len()).ok_or(())?;
            target.copy_from_slice(pixels);
            Ok(())
        },
    )
}

fn screens(importer: &Importer) -> Vec<Found> {
    importer.found::<display::Protocol>(DISPLAY_REQUIREMENT)
}

fn opened(screen: &Found) -> display::Client<SessionTransport> {
    component_startup::open::<display::Protocol>(screen, SIMPLEFB_REPLY_TIMEOUT)
        .expect("framebuffer opens")
        .client()
        .with_wait(SIMPLEFB_REPLY_TIMEOUT)
}

fn fill(
    surface: &Mapping,
    dimension: DisplayDimension,
    area: Area,
    color: impl Fn(u32, u32) -> u32,
) {
    let stride = dimension.surface_stride();
    let mut row = Vec::with_capacity(area.width as usize * SURFACE_PIXEL_BYTES as usize);
    for y in area.y..area.y + area.height {
        row.clear();
        for x in area.x..area.x + area.width {
            row.extend_from_slice(&color(x, y).to_le_bytes());
        }
        let offset = u64::from(y) * stride + u64::from(area.x) * u64::from(SURFACE_PIXEL_BYTES);
        surface
            .copy_in_at(offset as usize, &row)
            .expect("row inside the surface");
    }
}

fn channel(position: u32, side: u32) -> u32 {
    (u64::from(position) * SURFACE_CHANNEL_MAX / u64::from(side)) as u32
}

fn whole(dimension: DisplayDimension) -> Area {
    Area {
        x: 0,
        y: 0,
        width: dimension.width,
        height: dimension.height,
    }
}

#[kernel_test]
fn a_described_framebuffer_reports_its_dimension_and_needs_a_window_of_its_frame() {
    let phone = framebuffer("/phone@1000").expect("framebuffer accepted");
    let frame = 4320 * 2340;

    kernel_tests::kassert_eq!(
        phone.dimension(),
        DisplayDimension {
            width: 1080,
            height: 2340
        }
    );
    kernel_tests::kassert_eq!(phone.stride(), 4320);
    kernel_tests::kassert_eq!(phone.format(), "a8r8g8b8");
    kernel_tests::kassert_eq!(phone.fits(frame), Ok(()));
    kernel_tests::kassert_eq!(phone.fits(frame - 1), Err(Refusal::Malformed));
}

#[kernel_test]
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
        kernel_tests::kassert_eq!(framebuffer(path), Err(refusal));
    }
}

#[kernel_test]
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

    kernel_tests::kassert_eq!(
        padded.dimension(),
        DisplayDimension {
            width: 70,
            height: 3
        }
    );
    kernel_tests::kassert_eq!(
        unpadded.dimension(),
        DisplayDimension {
            width: 4,
            height: 2
        }
    );
    kernel_tests::kassert_eq!(
        written(&padded, 288 * 3, area),
        (288 + A8R8G8B8_PIXEL_BYTES..288 + 3 * A8R8G8B8_PIXEL_BYTES).collect::<Vec<_>>()
    );
    kernel_tests::kassert_eq!(
        written(&unpadded, 16 * 2, area),
        (16 + A8R8G8B8_PIXEL_BYTES..16 + 3 * A8R8G8B8_PIXEL_BYTES).collect::<Vec<_>>()
    );
    kernel_tests::kassert_eq!(
        written(&packed, 16 * 2, area),
        (16 + R8G8B8_PIXEL_BYTES..16 + 3 * R8G8B8_PIXEL_BYTES).collect::<Vec<_>>()
    );
}

#[kernel_test]
fn each_binding_format_encodes_a_surface_pixel_with_an_opaque_alpha() {
    let encoded: [(&str, &[u8]); 11] = [
        ("r5g6b5", &[0x10, 0xF8]),
        ("r5g5b5a1", &[0x21, 0xF8]),
        ("x1r5g5b5", &[0x10, 0x7C]),
        ("a1r5g5b5", &[0x10, 0xFC]),
        ("r8g8b8", &[0x80, 0x00, 0xFF]),
        ("x8r8g8b8", &[0x80, 0x00, 0xFF, 0x00]),
        ("a8r8g8b8", &[0x80, 0x00, 0xFF, 0xFF]),
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
        let stride = [expected.len() as u32];
        let blob = fdt_test::build(&[
            Token::Node("pixel@0"),
            Token::Strings("compatible", &[SIMPLE_FRAMEBUFFER_COMPATIBLE]),
            Token::Cells("width", &[1]),
            Token::Cells("height", &[1]),
            Token::Cells("stride", &stride),
            Token::Strings("format", &[format]),
            Token::End,
        ]);
        let description = Description::new(&blob, 0).expect("tree parses");
        let device = DeviceId::new("/pixel@0").expect("absolute path");
        let framebuffer = Framebuffer::open(&description, &device).expect("format accepted");
        let mut screen = alloc::vec![UNTOUCHED_SCREEN_BYTE; expected.len()];

        let presented = present(
            &framebuffer,
            pixel,
            &SURFACE_FULL_RED_HALF_BLUE.to_le_bytes(),
            &mut screen,
        );

        kernel_tests::kassert_eq!(presented, Ok(()));
        kernel_tests::kassert_eq!((format, screen.as_slice()), (format, expected));
    }
}

#[kernel_test]
fn present_writes_the_area_opaque_and_keeps_the_neighbours_and_the_row_padding() {
    let padded = framebuffer("/padded@2000").expect("framebuffer accepted");
    let surface: Vec<u8> = (0..70 * 3 * SURFACE_PIXEL_BYTES as usize)
        .map(|byte| byte as u8)
        .collect();
    let mut screen = alloc::vec![UNTOUCHED_SCREEN_BYTE; 288 * 3];
    let area = Area {
        x: 1,
        y: 1,
        width: 66,
        height: 1,
    };

    let presented = present(&padded, area, &surface, &mut screen);

    kernel_tests::kassert_eq!(presented, Ok(()));
    for (offset, &byte) in screen.iter().enumerate() {
        let (row, column) = (offset / 288, offset % 288);
        let inside =
            row == 1 && (A8R8G8B8_PIXEL_BYTES..67 * A8R8G8B8_PIXEL_BYTES).contains(&column);
        let expected = match (inside, column % A8R8G8B8_PIXEL_BYTES) {
            (false, _) => UNTOUCHED_SCREEN_BYTE,
            (true, 3) => OPAQUE_ALPHA,
            (true, _) => surface[70 * SURFACE_PIXEL_BYTES as usize + column],
        };
        kernel_tests::kassert_eq!(byte, expected);
    }
}

#[kernel_test]
fn present_of_an_area_past_the_screen_is_out_of_bounds_before_any_copy() {
    let padded = framebuffer("/padded@2000").expect("framebuffer accepted");
    let mut copies = 0;
    let past_edge = Area {
        x: 66,
        y: 0,
        width: 8,
        height: 1,
    };

    let presented = padded.present(
        past_edge,
        |_, _| {
            copies += 1;
            Ok::<_, ()>(())
        },
        |_, _| Ok(()),
    );

    kernel_tests::kassert_eq!(presented, Err(DisplayError::OutOfBounds));
    kernel_tests::kassert_eq!(copies, 0);
}

#[kernel_test]
fn each_described_framebuffer_shows_a_gradient_and_a_rectangle() {
    let composition = with_drivers();
    let importer = composition.importer(DISPLAY_IMPORTS);
    let framebuffers = screens(&importer);

    for found in &framebuffers {
        let screen = opened(found);
        let dimension = screen.dimension().expect("dimension call");
        let region = MemoryRegion::from(
            screen
                .acquire_surface()
                .expect("acquire_surface call")
                .expect("surface granted"),
        );
        let surface = region
            .map_full(UserMemFlags::ReadWrite)
            .expect("surface maps");
        let rectangle = Area {
            x: dimension.width / 4,
            y: dimension.height / 4,
            width: dimension.width / 2,
            height: dimension.height / 2,
        };

        fill(&surface, dimension, whole(dimension), |x, y| {
            (channel(x, dimension.width) << SURFACE_RED_SHIFT)
                | (channel(y, dimension.height) << SURFACE_GREEN_SHIFT)
        });
        let gradient = screen.present(whole(dimension)).expect("present call");
        fill(&surface, dimension, rectangle, |_, _| RECTANGLE_COLOR);
        let shown = screen.present(rectangle).expect("present call");

        kernel_tests::kassert_eq!(gradient, Ok(()));
        kernel_tests::kassert_eq!(shown, Ok(()));
        kernel_tests::kreport!("the framebuffer shows the gradient and the rectangle");
    }
    finish(composition);

    if framebuffers.is_empty() {
        kernel_tests::kskip!("no simple-framebuffer described");
    }
}

#[kernel_test]
fn surfaces_of_a_framebuffer_end_with_the_budget_and_return_with_their_sessions() {
    let composition = with_drivers();
    let importer = composition.importer(DISPLAY_IMPORTS);
    let framebuffers = screens(&importer);

    for found in &framebuffers {
        let mut screens = Vec::new();
        // Каждая поверхность занимает не меньше страницы бюджета драйвера.
        let refusal = (0..=SIMPLEFB_MEMORY / USERLAND_PAGE_SIZE).find_map(|_| {
            let screen = opened(found);
            let before = screen.present(whole(screen.dimension().expect("dimension call")));
            kernel_tests::kassert_eq!(before, Ok(Err(DisplayError::NoSurface)));
            match screen.acquire_surface().expect("acquire_surface call") {
                Ok(region) => {
                    screens.push((screen, MemoryRegion::from(region)));
                    None
                }
                Err(error) => Some(error),
            }
        });
        let held = screens.len();
        drop(screens);
        let reopened = opened(found);
        let regained = reopened
            .acquire_surface()
            .expect("acquire_surface call")
            .map(drop);

        kernel_tests::kassert_eq!(refusal, Some(DisplayError::NoSpace));
        kernel_tests::kassert!(held >= 1);
        kernel_tests::kassert_eq!(regained, Ok(()));
        kernel_tests::kreport!("the framebuffer held {} surfaces", held);
    }
    finish(composition);

    if framebuffers.is_empty() {
        kernel_tests::kskip!("no simple-framebuffer described");
    }
}
