#[test]
fn handle_kind_violations_do_not_compile() {
    trybuild::TestCases::new().compile_fail("tests/compile_fail/*.rs");
}
