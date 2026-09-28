use fdt_test::Token::{Cells, End, Node, Strings};

#[test]
fn tree_expands_into_tokens_in_traversal_order() {
    let tokens = fdt_test::tree! {
        #address-cells = <1>;
        interrupt-controller@8000000 {
            compatible = "vendor,a", "vendor,b";
            reg = <0x800_0000 0x1000>;
            "linux,phandle" = <3>;
            ranges;
            fb@1f000 {
                no-map;
            };
        };
        chosen {};
    };

    assert_eq!(
        tokens,
        [
            Cells("#address-cells", &[1]),
            Node("interrupt-controller@8000000"),
            Strings("compatible", &["vendor,a", "vendor,b"]),
            Cells("reg", &[0x800_0000, 0x1000]),
            Cells("linux,phandle", &[3]),
            Cells("ranges", &[]),
            Node("fb@1f000"),
            Cells("no-map", &[]),
            End,
            End,
            Node("chosen"),
            End,
        ]
    );
}
