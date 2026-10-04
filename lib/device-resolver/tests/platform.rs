#![allow(clippy::expect_used)]

use device_resolver::{
    ControllerBinding, ControllerBindings, ControllerId, DeviceInterrupt, DeviceMmio,
    DeviceTreePlatform, InterruptLine, InterruptSpecifier, PlatformDescription, ResolveError,
};
use fdt::devicetree::DeviceTree;
use fdt_test::Token::{Cells, End, Node, Strings};
use platform_abi::DeviceId;
use syscall::IrqTrigger;

const CONTROLLER_PHANDLE: u32 = 0x8001;

const TEST_CONTROLLER: &str = "test,controller";
const TEST_SPECIFIER_CELLS: usize = 2;
const TEST_TRIGGER_EDGE: u32 = 1;

const TEST_BINDINGS: ControllerBindings = ControllerBindings::new(&[ControllerBinding::new(
    &[TEST_CONTROLLER],
    decode_test_specifier,
)]);

const CLOCK: &str = "test,clock";
const SERIAL: &str = "test,serial";
const NESTED_CLOCK: &str = "test,soc-clock";
const BRIDGED_CLOCK: &str = "test,bridged-clock";
const UNMAPPED_CLOCK: &str = "test,unmapped-clock";

fn decode_test_specifier(specifier: InterruptSpecifier<'_>) -> Option<DeviceInterrupt> {
    if specifier.cell_count() != TEST_SPECIFIER_CELLS {
        return None;
    }
    let trigger = if specifier.cell(1)? == TEST_TRIGGER_EDGE {
        IrqTrigger::Edge
    } else {
        IrqTrigger::Level
    };
    Some(DeviceInterrupt {
        line: InterruptLine::new(
            ControllerId::PLATFORM,
            u16::try_from(specifier.cell(0)?).ok()?,
        ),
        trigger,
    })
}

fn platform_dtb(controller: &str) -> Vec<u8> {
    fdt_test::build(&[
        Cells("#address-cells", &[2]),
        Cells("#size-cells", &[2]),
        Cells("interrupt-parent", &[CONTROLLER_PHANDLE]),
        Node("intc@8000000"),
        Strings("compatible", &[controller]),
        Cells("#interrupt-cells", &[2]),
        Cells("phandle", &[CONTROLLER_PHANDLE]),
        End,
        Node("clock@9010000"),
        Strings("compatible", &[CLOCK, "test,primecell"]),
        Cells("reg", &[0, 0x0901_0000, 0, 0x1000]),
        Cells("interrupts", &[34, 0]),
        End,
        Node("clock@9020000"),
        Strings("compatible", &[CLOCK]),
        Cells("reg", &[0, 0x0902_0000, 0, 0x1000]),
        Cells("interrupts", &[35, 1]),
        End,
        Node("clock@9040000"),
        Strings("compatible", &[CLOCK]),
        Strings("status", &["disabled"]),
        Cells("reg", &[0, 0x0904_0000, 0, 0x1000]),
        Cells("interrupts", &[38, 0]),
        End,
        Node("serial@9000000"),
        Strings("compatible", &[SERIAL]),
        Cells("reg", &[0, 0x0900_0000, 0, 0x1000]),
        End,
        Node("soc"),
        Cells("#address-cells", &[2]),
        Cells("#size-cells", &[2]),
        Cells("ranges", &[]),
        Cells("interrupt-parent", &[CONTROLLER_PHANDLE]),
        Node("clock@9030000"),
        Strings("compatible", &[NESTED_CLOCK]),
        Cells("reg", &[0, 0x0903_0000, 0, 0x1000]),
        Cells("interrupts", &[36, 0]),
        End,
        End,
        Node("bridge"),
        Cells("#address-cells", &[2]),
        Cells("#size-cells", &[2]),
        Cells("ranges", &[0, 0, 0, 0x1000_0000, 0, 0x0010_0000]),
        Node("clock@40000"),
        Strings("compatible", &[BRIDGED_CLOCK]),
        Cells(
            "reg",
            &[0, 0x0004_0000, 0, 0x1000, 0, 0x0020_0000, 0, 0x1000],
        ),
        Cells("interrupts", &[37, 0]),
        End,
        End,
        Node("spmi"),
        Cells("#address-cells", &[2]),
        Cells("#size-cells", &[2]),
        Node("clock@40000"),
        Strings("compatible", &[UNMAPPED_CLOCK]),
        Cells("reg", &[0, 0x0004_0000, 0, 0x1000]),
        End,
        End,
    ])
}

fn id(path: &str) -> DeviceId {
    DeviceId::new(path).expect("path is a device id")
}

fn with_platform<R>(controller: &str, body: impl FnOnce(&DeviceTreePlatform<'_>) -> R) -> R {
    let bytes = platform_dtb(controller);
    let tree = DeviceTree::from_bytes(&bytes).expect("device tree parses");
    let platform = DeviceTreePlatform::new(&tree, TEST_BINDINGS);
    body(&platform)
}

#[test]
fn a_described_device_yields_its_line_and_window() {
    let clock = id("/clock@9010000");

    let (interrupt, mmio) = with_platform(TEST_CONTROLLER, |platform| {
        (platform.interrupt(&clock, 0), platform.mmio(&clock, 0))
    });

    assert_eq!(
        interrupt,
        Ok(DeviceInterrupt {
            line: InterruptLine::new(ControllerId::PLATFORM, 34),
            trigger: IrqTrigger::Level,
        })
    );
    assert_eq!(
        mmio,
        Ok(DeviceMmio {
            base: 0x0901_0000,
            size: 0x1000,
        })
    );
}

#[test]
fn devices_of_one_compatible_are_addressed_by_their_paths() {
    let second = id("/clock@9020000");

    let (interrupt, mmio) = with_platform(TEST_CONTROLLER, |platform| {
        (platform.interrupt(&second, 0), platform.mmio(&second, 0))
    });

    assert_eq!(
        interrupt,
        Ok(DeviceInterrupt {
            line: InterruptLine::new(ControllerId::PLATFORM, 35),
            trigger: IrqTrigger::Edge,
        })
    );
    assert_eq!(mmio.map(|window| window.base), Ok(0x0902_0000));
}

#[test]
fn an_absent_or_disabled_device_is_not_found() {
    let absent = id("/absent@0");
    let disabled = id("/clock@9040000");

    let (absent_interrupt, absent_mmio, disabled_mmio) =
        with_platform(TEST_CONTROLLER, |platform| {
            (
                platform.interrupt(&absent, 0),
                platform.mmio(&absent, 0),
                platform.mmio(&disabled, 0),
            )
        });

    assert_eq!(absent_interrupt, Err(ResolveError::DeviceNotFound));
    assert_eq!(absent_mmio, Err(ResolveError::DeviceNotFound));
    assert_eq!(disabled_mmio, Err(ResolveError::DeviceNotFound));
}

#[test]
fn a_device_without_interrupts_is_named_apart() {
    let serial = id("/serial@9000000");

    let interrupt = with_platform(TEST_CONTROLLER, |platform| platform.interrupt(&serial, 0));

    assert_eq!(interrupt, Err(ResolveError::NoInterrupts));
}

#[test]
fn a_resource_index_past_the_set_is_named_apart() {
    let clock = id("/clock@9010000");

    let (interrupt, mmio) = with_platform(TEST_CONTROLLER, |platform| {
        (platform.interrupt(&clock, 1), platform.mmio(&clock, 1))
    });

    assert_eq!(interrupt, Err(ResolveError::ResourceOutOfRange));
    assert_eq!(mmio, Err(ResolveError::ResourceOutOfRange));
}

#[test]
fn an_unknown_controller_is_an_error_and_not_a_default() {
    let clock = id("/clock@9010000");

    let (interrupt, mmio) = with_platform("test,absent-controller", |platform| {
        (platform.interrupt(&clock, 0), platform.mmio(&clock, 0))
    });

    assert_eq!(interrupt, Err(ResolveError::UnknownController));
    assert!(mmio.is_ok());
}

#[test]
fn a_device_behind_an_identity_bus_is_found_with_its_inherited_controller() {
    let nested = id("/soc/clock@9030000");

    let (interrupt, mmio) = with_platform(TEST_CONTROLLER, |platform| {
        (platform.interrupt(&nested, 0), platform.mmio(&nested, 0))
    });

    assert_eq!(
        interrupt,
        Ok(DeviceInterrupt {
            line: InterruptLine::new(ControllerId::PLATFORM, 36),
            trigger: IrqTrigger::Level,
        })
    );
    assert_eq!(
        mmio,
        Ok(DeviceMmio {
            base: 0x0903_0000,
            size: 0x1000,
        })
    );
}

#[test]
fn a_window_behind_a_translating_bus_gets_its_translated_base() {
    let bridged = id("/bridge/clock@40000");

    let (interrupt, mmio) = with_platform(TEST_CONTROLLER, |platform| {
        (platform.interrupt(&bridged, 0), platform.mmio(&bridged, 0))
    });

    assert_eq!(interrupt.map(|interrupt| interrupt.line.number()), Ok(37));
    assert_eq!(
        mmio,
        Ok(DeviceMmio {
            base: 0x1004_0000,
            size: 0x1000,
        })
    );
}

#[test]
fn a_window_outside_the_bus_ranges_is_untranslatable() {
    let bridged = id("/bridge/clock@40000");

    let mmio = with_platform(TEST_CONTROLLER, |platform| platform.mmio(&bridged, 1));

    assert_eq!(mmio, Err(ResolveError::Untranslatable));
}

#[test]
fn a_window_behind_a_bus_without_ranges_is_untranslatable() {
    let unmapped = id("/spmi/clock@40000");

    let mmio = with_platform(TEST_CONTROLLER, |platform| platform.mmio(&unmapped, 0));

    assert_eq!(mmio, Err(ResolveError::Untranslatable));
}
