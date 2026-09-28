//! Резолюция устройства платформы через реестр биндингов aarch64: specifier
//! PL031 в описании qemu virt - `GIC_SPI 2 IRQ_TYPE_LEVEL_HIGH`.

#![allow(clippy::expect_used)]

use device_resolver::{
    DeviceMmio, DeviceSelector, DeviceTreePlatform, PlatformDescription, ResolveError,
};
use device_resolver_aarch64::PLATFORM_BINDINGS;
use fdt::devicetree::DeviceTree;
use fdt_test::Token::{Cells, End, Node, Strings};
use syscall::IrqTrigger;

const GIC_PHANDLE: u32 = 0x8001;

const GIC: &str = "arm,gic-v3";
const PL031: &str = "arm,pl031";

const PL031_BASE: u64 = 0x0901_0000;
const PL031_SIZE: u64 = 0x1000;

const PL031_SPECIFIER: &[u32] = &[0, 2, 4];
const PL031_LINE: u16 = 34;

fn platform_dtb(controller: &str) -> Vec<u8> {
    let reg = [
        0,
        u32::try_from(PL031_BASE).expect("window base fits a cell"),
        0,
        u32::try_from(PL031_SIZE).expect("window size fits a cell"),
    ];
    fdt_test::build(&[
        Cells("#address-cells", &[2]),
        Cells("#size-cells", &[2]),
        Cells("interrupt-parent", &[GIC_PHANDLE]),
        Node("intc@8000000"),
        Strings("compatible", &[controller]),
        Cells("#interrupt-cells", &[3]),
        Cells("phandle", &[GIC_PHANDLE]),
        End,
        Node("pl031@9010000"),
        Strings("compatible", &[PL031, "arm,primecell"]),
        Cells("reg", &reg),
        Cells("interrupts", PL031_SPECIFIER),
        End,
    ])
}

fn with_platform<R>(controller: &str, body: impl FnOnce(&DeviceTreePlatform<'_>) -> R) -> R {
    let bytes = platform_dtb(controller);
    let tree = DeviceTree::from_bytes(&bytes).expect("device tree parses");
    let platform = DeviceTreePlatform::new(&tree, PLATFORM_BINDINGS);
    body(&platform)
}

fn selector(compatible: &str) -> DeviceSelector<'_> {
    DeviceSelector {
        compatible,
        instance: 0,
    }
}

#[test]
fn a_platform_device_resolves_through_the_gic_binding() {
    with_platform(GIC, |platform| {
        let interrupt = platform
            .interrupt(selector(PL031), 0)
            .expect("pl031 line is described");

        assert_eq!(interrupt.line.number(), PL031_LINE);
        assert_eq!(interrupt.trigger, IrqTrigger::Level);
        assert_eq!(
            platform.mmio(selector(PL031), 0),
            Ok(DeviceMmio {
                base: PL031_BASE,
                size: PL031_SIZE,
            })
        );
    });
}

#[test]
fn a_controller_outside_the_platform_registry_is_an_error() {
    with_platform("riscv,plic0", |platform| {
        assert_eq!(
            platform.interrupt(selector(PL031), 0),
            Err(ResolveError::UnknownController)
        );
    });
}
