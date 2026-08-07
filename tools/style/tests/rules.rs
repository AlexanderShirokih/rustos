use std::path::Path;

use style_tool::{Config, Rule, check_one, compiled};

fn rule(id: &str) -> Rule {
    compiled()
        .into_iter()
        .find(|rule| rule.meta.id == id)
        .unwrap_or_else(|| panic!("compiled catalog has no rule {id}"))
}

fn declared(id: &str) -> Rule {
    let catalog = compiled();
    let text =
        std::fs::read_to_string(Path::new(env!("CARGO_MANIFEST_DIR")).join("../../style.toml"))
            .expect("read style.toml");
    let Ok(mut config) = Config::parse(&text, &catalog) else {
        panic!("style.toml must load");
    };
    config
        .catalog(Vec::new())
        .into_iter()
        .find(|rule| rule.meta.id == id)
        .unwrap_or_else(|| panic!("style.toml declares no rule {id}"))
}

fn check(rule: &Rule, source: &str) -> Vec<String> {
    check_one(rule, Path::new("kernel/demo/src/demo.rs"), source)
}

#[test]
fn doc_on_private_declaration_is_reported() {
    let hits = check(&rule("doc_on_declaration"), "/// Doc.\nfn helper() {}\n");
    assert_eq!(hits.len(), 1, "{hits:?}");
}

#[test]
fn doc_on_public_declaration_is_accepted() {
    let hits = check(
        &rule("doc_on_declaration"),
        "/// Doc.\npub fn helper() {}\n",
    );
    assert!(hits.is_empty(), "{hits:?}");
}

#[test]
fn doc_survives_attributes_between_it_and_the_declaration() {
    let hits = check(
        &rule("doc_on_declaration"),
        "/// Doc.\n#[derive(Debug)]\n#[repr(C)]\nstruct Hidden;\n",
    );
    assert_eq!(hits.len(), 1, "{hits:?}");
}

#[test]
fn variant_of_a_public_enum_inherits_visibility() {
    let hits = check(
        &rule("doc_on_declaration"),
        "pub enum Kind {\n    /// Doc.\n    First,\n}\n",
    );
    assert!(hits.is_empty(), "{hits:?}");
}

#[test]
fn variant_of_a_private_enum_is_reported() {
    let hits = check(
        &rule("doc_on_declaration"),
        "enum Kind {\n    /// Doc.\n    First,\n}\n",
    );
    assert_eq!(hits.len(), 1, "{hits:?}");
}

#[test]
fn field_of_a_public_variant_inherits_visibility() {
    let source = concat!(
        "pub enum Kind {\n",
        "    First {\n",
        "        /// Doc.\n",
        "        span: usize,\n",
        "    },\n",
        "}\n"
    );
    let hits = check(&rule("doc_on_declaration"), source);
    assert!(hits.is_empty(), "{hits:?}");
}

#[test]
fn field_of_a_private_variant_is_reported() {
    let source = concat!(
        "enum Kind {\n",
        "    First {\n",
        "        /// Doc.\n",
        "        span: usize,\n",
        "    },\n",
        "}\n"
    );
    let hits = check(&rule("doc_on_declaration"), source);
    assert_eq!(hits.len(), 1, "{hits:?}");
}

#[test]
fn field_of_a_public_struct_still_needs_its_own_pub() {
    let source = concat!(
        "pub struct Frame {\n",
        "    /// Doc.\n",
        "    span: usize,\n",
        "}\n"
    );
    let hits = check(&rule("doc_on_declaration"), source);
    assert_eq!(hits.len(), 1, "{hits:?}");
}

#[test]
fn comment_on_a_field_of_a_public_variant_inherits_visibility() {
    let source = concat!(
        "pub enum Kind {\n",
        "    First {\n",
        "        // Span of the record.\n",
        "        span: usize,\n",
        "    },\n",
        "}\n"
    );
    let hits = check(&rule("comment_on_declaration"), source);
    assert!(hits.is_empty(), "{hits:?}");
}

#[test]
fn exported_macro_counts_as_public() {
    let hits = check(
        &rule("doc_on_declaration"),
        "/// Doc.\n#[macro_export]\nmacro_rules! shout {\n    () => {};\n}\n",
    );
    assert!(hits.is_empty(), "{hits:?}");
}

#[test]
fn unexported_macro_is_reported() {
    let hits = check(
        &rule("doc_on_declaration"),
        "/// Doc.\nmacro_rules! shout {\n    () => {};\n}\n",
    );
    assert_eq!(hits.len(), 1, "{hits:?}");
}

#[test]
fn derive_of_a_command_line_parser_carries_its_own_help() {
    let source = concat!(
        "#[derive(Parser)]\n",
        "struct Cli {\n",
        "    /// Path to the device spec\n",
        "    spec: String,\n",
        "}\n"
    );
    let hits = check(&rule("doc_on_declaration"), source);
    assert!(hits.is_empty(), "{hits:?}");
}

#[test]
fn declaration_inside_a_macro_body_is_not_judged() {
    let source = concat!(
        "macro_rules! define {\n",
        "    () => {\n",
        "        /// Doc.\n",
        "        fn generated() {}\n",
        "    };\n",
        "}\n"
    );
    let hits = check(&rule("doc_on_declaration"), source);
    assert!(hits.is_empty(), "{hits:?}");
}

#[test]
fn doc_on_a_test_is_left_to_its_own_rule() {
    let source = "/// Regression guard.\n#[test]\nfn scenario() {}\n";
    let hits = check(&rule("doc_on_declaration"), source);
    assert!(hits.is_empty(), "{hits:?}");
}

#[test]
fn comment_attached_to_a_private_declaration_is_reported() {
    let hits = check(
        &rule("comment_on_declaration"),
        "// Helper of the parser.\nfn helper() {}\n",
    );
    assert_eq!(hits.len(), 1, "{hits:?}");
}

#[test]
fn comment_attached_to_a_public_declaration_is_accepted() {
    let hits = check(
        &rule("comment_on_declaration"),
        "// Helper of the parser.\npub fn helper() {}\n",
    );
    assert!(hits.is_empty(), "{hits:?}");
}

#[test]
fn comment_separated_by_a_blank_line_stands_on_its_own() {
    let hits = check(
        &rule("comment_on_declaration"),
        "// Section of the file.\n\nfn helper() {}\n",
    );
    assert!(hits.is_empty(), "{hits:?}");
}

#[test]
fn comment_survives_attributes_between_it_and_the_declaration() {
    let hits = check(
        &rule("comment_on_declaration"),
        "#[derive(Debug)]\n// Layout of the frame.\nstruct Hidden;\n",
    );
    assert_eq!(hits.len(), 1, "{hits:?}");
}

#[test]
fn safety_contract_of_a_private_declaration_is_accepted() {
    let hits = check(
        &rule("comment_on_declaration"),
        "// SAFETY: the caller holds the mapping.\nunsafe fn raw() {}\n",
    );
    assert!(hits.is_empty(), "{hits:?}");
}

#[test]
fn suppression_directive_is_not_prose_of_its_own() {
    let hits = check(
        &rule("comment_on_declaration"),
        "// style:allow(comment_width) fixture\nfn helper() {}\n",
    );
    assert!(hits.is_empty(), "{hits:?}");
}

#[test]
fn comment_trailing_the_line_it_constrains_is_accepted() {
    let hits = check(
        &rule("comment_on_declaration"),
        "const LIMIT: usize = 4; // the device counts frames from one\n",
    );
    assert!(hits.is_empty(), "{hits:?}");
}

#[test]
fn comment_on_a_test_is_left_to_its_own_rule() {
    let hits = check(
        &rule("comment_on_declaration"),
        "// Guards the badge rollover.\n#[test]\nfn scenario() {}\n",
    );
    assert!(hits.is_empty(), "{hits:?}");
}

#[test]
fn short_doc_on_a_test_fits_the_regression_budget() {
    let source = "/// Guards the badge rollover.\n#[test]\nfn scenario() {}\n";
    let hits = check(&rule("comment_on_test"), source);
    assert!(hits.is_empty(), "{hits:?}");
}

#[test]
fn long_doc_on_a_test_exceeds_the_regression_budget() {
    let source = concat!(
        "/// Line one.\n",
        "/// Line two.\n",
        "/// Line three.\n",
        "#[test]\n",
        "fn scenario() {}\n"
    );
    let hits = check(&rule("comment_on_test"), source);
    assert_eq!(hits.len(), 1, "{hits:?}");
}

#[test]
fn plain_comment_on_a_test_spends_the_same_budget() {
    let source = concat!(
        "// Line one.\n",
        "// Line two.\n",
        "// Line three.\n",
        "#[test]\n",
        "fn scenario() {}\n"
    );
    let hits = check(&rule("comment_on_test"), source);
    assert_eq!(hits.len(), 1, "{hits:?}");
}

#[test]
fn cyrillic_literal_inside_a_macro_call_is_reported() {
    let source = "fn boom() {\n    panic!(\"\u{43e}\u{448}\u{438}\u{431}\u{43a}\u{430}\");\n}\n";
    let hits = check(&rule("cyrillic_in_literal"), source);
    assert_eq!(hits.len(), 1, "{hits:?}");
}

#[test]
fn cyrillic_comment_is_not_a_literal() {
    let source = "// \u{43a}\u{43e}\u{43c}\u{43c}\u{435}\u{43d}\u{442}\u{430}\u{440}\u{438}\u{439}\nfn quiet() {}\n";
    let hits = check(&rule("cyrillic_in_literal"), source);
    assert!(hits.is_empty(), "{hits:?}");
}

#[test]
fn comment_inside_a_raw_string_is_not_a_comment() {
    let source = "pub const ASM: &str = r#\"\n    // \u{43d}\u{435} \u{43a}\u{43e}\u{43c}\u{43c}\u{435}\u{43d}\u{442}\u{430}\u{440}\u{438}\u{439}\n\"#;\n";
    let hits = check(&declared("latin_apostrophe"), source);
    assert!(hits.is_empty(), "{hits:?}");
}

#[test]
fn apostrophe_declension_is_reported() {
    let source = "// \u{441}\u{447}\u{451}\u{442}\u{447}\u{438}\u{43a} handle'\u{43e}\u{432}\nfn count() {}\n";
    let hits = check(&declared("latin_apostrophe"), source);
    assert_eq!(hits.len(), 1, "{hits:?}");
}

#[test]
fn lifetime_is_not_an_apostrophe_declension() {
    let source = "// \u{436}\u{438}\u{432}\u{451}\u{442} 'static \u{438} 'a\nfn borrow() {}\n";
    let hits = check(&declared("latin_apostrophe"), source);
    assert!(hits.is_empty(), "{hits:?}");
}

#[test]
fn wide_comment_line_is_reported() {
    let wide = "x".repeat(120);
    let source = format!("// {wide}\nfn quiet() {{}}\n");
    let hits = check(&rule("comment_width"), &source);
    assert_eq!(hits.len(), 1, "{hits:?}");
}

#[test]
fn wide_code_line_is_not_a_comment() {
    let wide = "x".repeat(120);
    let source = format!("pub const NAME: &str = \"{wide}\";\n");
    let hits = check(&rule("comment_width"), &source);
    assert!(hits.is_empty(), "{hits:?}");
}
