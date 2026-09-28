//! Обход описания и отказы резолвера на биндинге, объявленном самим тестом:
//! нейтральному крейту незачем знать контроллеры платформы. Первая ячейка
//! specifier - номер линии, вторая - триггер.

#![allow(clippy::expect_used)]

use device_resolver::{
    ControllerBinding, ControllerBindings, ControllerId, DeviceInterrupt, DeviceMmio,
    DeviceSelector, DeviceTreePlatform, InterruptLine, InterruptSpecifier, PlatformDescription,
    ResolveError,
};
use fdt::devicetree::DeviceTree;
use fdt_test::Token::{Cells, End, Node, Strings};
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

fn selector(compatible: &str, instance: u16) -> DeviceSelector<'_> {
    DeviceSelector {
        compatible,
        instance,
    }
}

fn with_platform<R>(controller: &str, body: impl FnOnce(&DeviceTreePlatform<'_>) -> R) -> R {
    let bytes = platform_dtb(controller);
    let tree = DeviceTree::from_bytes(&bytes).expect("device tree parses");
    let platform = DeviceTreePlatform::new(&tree, TEST_BINDINGS);
    body(&platform)
}

#[test]
fn a_described_device_yields_its_line_and_window() {
    with_platform(TEST_CONTROLLER, |platform| {
        let interrupt = platform
            .interrupt(selector(CLOCK, 0), 0)
            .expect("clock line is described");
        assert_eq!(interrupt.line.number(), 34);
        assert_eq!(interrupt.line.controller(), ControllerId::PLATFORM);
        assert_eq!(interrupt.trigger, IrqTrigger::Level);

        assert_eq!(
            platform.mmio(selector(CLOCK, 0), 0),
            Ok(DeviceMmio {
                base: 0x0901_0000,
                size: 0x1000,
            })
        );
    });
}

#[test]
fn the_instance_index_picks_among_devices_of_one_compatible() {
    with_platform(TEST_CONTROLLER, |platform| {
        let second = platform
            .interrupt(selector(CLOCK, 1), 0)
            .expect("second clock is described");

        assert_eq!(second.line.number(), 35);
        assert_eq!(second.trigger, IrqTrigger::Edge);
        assert_eq!(
            platform
                .mmio(selector(CLOCK, 1), 0)
                .map(|window| window.base),
            Ok(0x0902_0000)
        );
    });
}

#[test]
fn a_device_outside_the_description_is_named_apart() {
    with_platform(TEST_CONTROLLER, |platform| {
        assert_eq!(
            platform.interrupt(selector("test,absent-device", 0), 0),
            Err(ResolveError::DeviceNotFound)
        );
        assert_eq!(
            platform.mmio(selector("test,absent-device", 0), 0),
            Err(ResolveError::DeviceNotFound)
        );
        // Экземпляр за концом набора совместимых - тот же отказ поиска.
        assert_eq!(
            platform.interrupt(selector(CLOCK, 2), 0),
            Err(ResolveError::DeviceNotFound)
        );
    });
}

#[test]
fn a_device_without_interrupts_is_named_apart() {
    with_platform(TEST_CONTROLLER, |platform| {
        assert_eq!(
            platform.interrupt(selector(SERIAL, 0), 0),
            Err(ResolveError::NoInterrupts)
        );
    });
}

#[test]
fn a_resource_index_past_the_set_is_named_apart() {
    with_platform(TEST_CONTROLLER, |platform| {
        assert_eq!(
            platform.interrupt(selector(CLOCK, 0), 1),
            Err(ResolveError::ResourceOutOfRange)
        );
        assert_eq!(
            platform.mmio(selector(CLOCK, 0), 1),
            Err(ResolveError::ResourceOutOfRange)
        );
    });
}

#[test]
fn an_unknown_controller_is_an_error_and_not_a_default() {
    with_platform("test,absent-controller", |platform| {
        assert_eq!(
            platform.interrupt(selector(CLOCK, 0), 0),
            Err(ResolveError::UnknownController)
        );
        // Окна устройства толкования прерываний не требуют.
        assert!(platform.mmio(selector(CLOCK, 0), 0).is_ok());
    });
}

#[test]
fn a_device_behind_an_identity_bus_is_found_with_its_inherited_controller() {
    with_platform(TEST_CONTROLLER, |platform| {
        let interrupt = platform
            .interrupt(selector(NESTED_CLOCK, 0), 0)
            .expect("nested device is described");
        assert_eq!(interrupt.line.number(), 36);
        assert_eq!(interrupt.trigger, IrqTrigger::Level);

        assert_eq!(
            platform.mmio(selector(NESTED_CLOCK, 0), 0),
            Ok(DeviceMmio {
                base: 0x0903_0000,
                size: 0x1000,
            })
        );
    });
}

#[test]
fn a_window_behind_a_translating_bus_gets_its_translated_base() {
    with_platform(TEST_CONTROLLER, |platform| {
        // Линия шиной не переносится и читается наравне с остальными.
        assert_eq!(
            platform
                .interrupt(selector(BRIDGED_CLOCK, 0), 0)
                .map(|interrupt| interrupt.line.number()),
            Ok(37)
        );
        assert_eq!(
            platform.mmio(selector(BRIDGED_CLOCK, 0), 0),
            Ok(DeviceMmio {
                base: 0x1004_0000,
                size: 0x1000,
            })
        );
    });
}

#[test]
fn a_window_outside_the_bus_ranges_is_untranslatable() {
    with_platform(TEST_CONTROLLER, |platform| {
        assert_eq!(
            platform.mmio(selector(BRIDGED_CLOCK, 0), 1),
            Err(ResolveError::Untranslatable)
        );
    });
}

#[test]
fn a_window_behind_a_bus_without_ranges_is_untranslatable() {
    with_platform(TEST_CONTROLLER, |platform| {
        assert_eq!(
            platform.mmio(selector(UNMAPPED_CLOCK, 0), 0),
            Err(ResolveError::Untranslatable)
        );
    });
}
