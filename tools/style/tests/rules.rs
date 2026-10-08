#![allow(clippy::expect_used, clippy::panic)]

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
fn a_path_test_attribute_with_a_profile_marks_a_test() {
    let source = concat!(
        "/// Line one.\n",
        "/// Line two.\n",
        "/// Line three.\n",
        "#[test_harness::test(bench)]\n",
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
    let source = "fn boom() {\n    panic!(\"ошибка\");\n}\n";
    let hits = check(&rule("cyrillic_in_literal"), source);
    assert_eq!(hits.len(), 1, "{hits:?}");
}

#[test]
fn cyrillic_comment_is_not_a_literal() {
    let source = "// комментарий\nfn quiet() {}\n";
    let hits = check(&rule("cyrillic_in_literal"), source);
    assert!(hits.is_empty(), "{hits:?}");
}

#[test]
fn comment_inside_a_raw_string_is_not_a_comment() {
    let source = "pub const ASM: &str = r#\"\n    // не комментарий\n\"#;\n";
    let hits = check(&declared("latin_apostrophe"), source);
    assert!(hits.is_empty(), "{hits:?}");
}

#[test]
fn apostrophe_declension_is_reported() {
    let source = "// счётчик handle'ов\nfn count() {}\n";
    let hits = check(&declared("latin_apostrophe"), source);
    assert_eq!(hits.len(), 1, "{hits:?}");
}

#[test]
fn lifetime_is_not_an_apostrophe_declension() {
    let source = "// живёт 'static и 'a\nfn borrow() {}\n";
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

#[test]
fn semicolon_between_sentences_is_reported() {
    let source = "/// Первое; второе.\npub fn thing() {}\n";
    let hits = check(&rule("semicolon_in_prose"), source);
    assert_eq!(hits.len(), 1, "{hits:?}");
}

#[test]
fn semicolon_closing_a_list_item_is_accepted() {
    let source = "/// - первое;\n/// - второе.\npub fn thing() {}\n";
    let hits = check(&rule("semicolon_in_prose"), source);
    assert!(hits.is_empty(), "{hits:?}");
}

#[test]
fn causal_connective_is_reported() {
    let source = "/// Держит слот, поэтому очередь не растёт.\npub fn thing() {}\n";
    let hits = check(&declared("causal_connective"), source);
    assert_eq!(hits.len(), 1, "{hits:?}");
}

#[test]
fn statement_without_a_reason_is_accepted() {
    let source = "/// Держит слот очереди.\npub fn thing() {}\n";
    let hits = check(&declared("causal_connective"), source);
    assert!(hits.is_empty(), "{hits:?}");
}

#[test]
fn definition_through_negation_is_reported() {
    let source = "/// Серверная сторона, а не клиентская.\npub fn thing() {}\n";
    let hits = check(&declared("definition_by_negation"), source);
    assert_eq!(hits.len(), 1, "{hits:?}");
}

#[test]
fn naming_the_thing_alone_is_accepted() {
    let source = "/// Серверная сторона порта.\npub fn thing() {}\n";
    let hits = check(&declared("definition_by_negation"), source);
    assert!(hits.is_empty(), "{hits:?}");
}

#[test]
fn subjunctive_mood_is_reported() {
    let source = "/// Хук снял бы учёт.\npub fn thing() {}\n";
    let hits = check(&declared("subjunctive_mood"), source);
    assert_eq!(hits.len(), 1, "{hits:?}");
}

#[test]
fn present_tense_is_accepted() {
    let source = "/// Хук снимает учёт.\npub fn thing() {}\n";
    let hits = check(&declared("subjunctive_mood"), source);
    assert!(hits.is_empty(), "{hits:?}");
}

#[test]
fn tuple_in_a_public_signature_is_reported() {
    let hits = check(
        &rule("tuple_in_public_signature"),
        "pub fn split() -> (u8, u8) {\n    (0, 0)\n}\n",
    );
    assert_eq!(hits.len(), 1, "{hits:?}");
}

#[test]
fn tuple_in_a_private_signature_is_accepted() {
    let hits = check(
        &rule("tuple_in_public_signature"),
        "fn split() -> (u8, u8) {\n    (0, 0)\n}\n",
    );
    assert!(hits.is_empty(), "{hits:?}");
}

#[test]
fn tuple_in_a_test_helper_is_out_of_scope() {
    let hits = check_one(
        &rule("tuple_in_public_signature"),
        Path::new("kernel/demo/src/test_util.rs"),
        "pub fn split() -> (u8, u8) {\n    (0, 0)\n}\n",
    );
    assert!(hits.is_empty(), "{hits:?}");
}

#[test]
fn expect_in_kernel_code_is_reported() {
    let hits = check(
        &rule("expect_in_kernel"),
        "pub fn install(slot: Option<u8>) -> u8 {\n    slot.expect(\"installed\")\n}\n",
    );
    assert_eq!(hits.len(), 1, "{hits:?}");
}

#[test]
fn expect_outside_the_kernel_is_out_of_scope() {
    let hits = check_one(
        &rule("expect_in_kernel"),
        Path::new("lib/demo/src/demo.rs"),
        "pub fn install(slot: Option<u8>) -> u8 {\n    slot.expect(\"installed\")\n}\n",
    );
    assert!(hits.is_empty(), "{hits:?}");
}

fn check_root(source: &str) -> Vec<String> {
    check_one(
        &rule("code_in_crate_root"),
        Path::new("lib/demo/src/lib.rs"),
        source,
    )
}

#[test]
fn declarations_and_reexports_in_the_crate_root_are_accepted() {
    let hits = check_root(concat!(
        "//! Demo.\n",
        "#![no_std]\n",
        "#[cfg(test)]\n",
        "extern crate std;\n",
        "mod mode;\n",
        "pub mod protocol;\n",
        "use core::fmt;\n",
        "pub use mode::Mode;\n",
    ));
    assert!(hits.is_empty(), "{hits:?}");
}

#[test]
fn a_definition_in_the_crate_root_is_reported() {
    let hits = check_root(concat!(
        "mod mode;\n",
        "pub const SIZE: u32 = 4;\n",
        "pub struct Mode;\n",
        "impl Mode {}\n",
        "fn helper() {}\n",
        "macro_rules! demo { () => {} }\n",
    ));
    assert_eq!(hits.len(), 5, "{hits:?}");
}

#[test]
fn a_macro_invocation_in_the_crate_root_is_reported() {
    let hits = check_root("ipc::protocol! {\n    pub mod display {}\n}\n");
    assert_eq!(hits.len(), 1, "{hits:?}");
}

#[test]
fn an_inline_module_in_the_crate_root_is_reported() {
    let hits = check_root("#[cfg(test)]\nmod tests {\n    #[test]\n    fn case() {}\n}\n");
    assert_eq!(hits.len(), 1, "{hits:?}");
}

#[test]
fn a_procedural_macro_entry_point_stays_in_the_crate_root() {
    let hits = check_root(concat!(
        "#[proc_macro_attribute]\n",
        "pub fn test(attr: TokenStream, item: TokenStream) -> TokenStream {\n",
        "    item\n",
        "}\n",
    ));
    assert!(hits.is_empty(), "{hits:?}");
}

#[test]
fn a_definition_outside_the_crate_root_is_out_of_scope() {
    let hits = check_one(
        &rule("code_in_crate_root"),
        Path::new("lib/demo/src/mode.rs"),
        "pub struct Mode;\n",
    );
    assert!(hits.is_empty(), "{hits:?}");
}

#[test]
fn fronted_negation_is_reported() {
    let hits = check(
        &declared("fronted_negation"),
        "/// Приёмника у неё нет.\npub fn thing() {}\n",
    );
    assert_eq!(hits.len(), 1, "{hits:?}");
}

#[test]
fn direct_order_technical_sentences_are_accepted() {
    let hits = check(
        &declared("fronted_negation"),
        "/// У процесса два порта. Сессия не даёт приёма.\npub fn thing() {}\n",
    );
    assert!(hits.is_empty(), "{hits:?}");
}

#[test]
fn subject_first_negation_is_accepted() {
    let hits = check(
        &declared("fronted_negation"),
        "/// У неё нет приёмника.\npub fn thing() {}\n",
    );
    assert!(hits.is_empty(), "{hits:?}");
}

#[test]
fn subject_first_predicate_of_absence_is_accepted() {
    let hits = check(
        &declared("fronted_negation"),
        "/// Буфер не нужен. Эта операция не требуется.\npub fn thing() {}\n",
    );
    assert!(hits.is_empty(), "{hits:?}");
}

#[test]
fn short_reply_is_accepted() {
    let hits = check(
        &declared("fronted_negation"),
        "/// Пока нет.\npub fn thing() {}\n",
    );
    assert!(hits.is_empty(), "{hits:?}");
}

#[test]
fn an_excepted_sentence_leaves_the_next_one_of_the_paragraph_reported() {
    let hits = check(
        &declared("fronted_negation"),
        "/// Пока нет. Поддержки этой функции в системе нет.\npub fn thing() {}\n",
    );
    assert_eq!(hits.len(), 1, "{hits:?}");
}

#[test]
fn paragraph_target_joins_a_sentence_split_across_two_doc_lines() {
    let source = concat!(
        "/// Такого узла по пути\n",
        "/// нет.\n",
        "pub fn thing() {}\n"
    );
    let hits = check(&declared("fronted_negation"), source);
    assert_eq!(hits.len(), 1, "{hits:?}");
}

#[test]
fn blank_doc_comment_line_breaks_the_paragraph() {
    let source = concat!(
        "/// Такого узла по пути\n",
        "///\n",
        "/// нет.\n",
        "pub fn thing() {}\n"
    );
    let hits = check(&declared("fronted_negation"), source);
    assert!(hits.is_empty(), "{hits:?}");
}

#[test]
fn postposed_sam_is_reported() {
    let hits = check(
        &declared("postposed_sam"),
        "/// Поток отмечает точку сам.\npub fn thing() {}\n",
    );
    assert_eq!(hits.len(), 1, "{hits:?}");
}

#[test]
fn sam_before_the_verb_is_accepted() {
    let hits = check(
        &declared("postposed_sam"),
        "/// Поток сам отмечает точку.\npub fn thing() {}\n",
    );
    assert!(hits.is_empty(), "{hits:?}");
}

#[test]
fn colon_justification_is_reported() {
    let hits = check(
        &declared("colon_justification"),
        "/// Порт-таймер заводится один на всё ожидание: приёмник ему не нужен.\npub fn thing() {}\n",
    );
    assert_eq!(hits.len(), 1, "{hits:?}");
}

#[test]
fn label_before_the_colon_is_accepted() {
    let hits = check(
        &declared("colon_justification"),
        "/// Аргументы: путь не абсолютный.\npub fn thing() {}\n",
    );
    assert!(hits.is_empty(), "{hits:?}");
}

#[test]
fn label_in_the_second_sentence_of_a_paragraph_is_accepted() {
    let hits = check(
        &declared("colon_justification"),
        "/// Создаёт объект. Аргументы: путь не абсолютный.\npub fn thing() {}\n",
    );
    assert!(hits.is_empty(), "{hits:?}");
}

#[test]
fn enumeration_after_the_colon_without_a_verb_is_accepted() {
    let hits = check(
        &declared("colon_justification"),
        "/// Три уровня: ядро, библиотека, пользователь.\npub fn thing() {}\n",
    );
    assert!(hits.is_empty(), "{hits:?}");
}

#[test]
fn colon_inside_backticks_is_not_a_clause_boundary() {
    let hits = check(
        &declared("colon_justification"),
        "/// См. `Foo: bar` пример.\npub fn thing() {}\n",
    );
    assert!(hits.is_empty(), "{hits:?}");
}

#[test]
fn a_verb_in_the_next_sentence_does_not_justify_an_earlier_colon() {
    let hits = check(
        &declared("colon_justification"),
        "/// Значение получено: код. Автор проверяет результат отдельно.\npub fn thing() {}\n",
    );
    assert!(hits.is_empty(), "{hits:?}");
}

#[test]
fn animistic_verb_is_reported() {
    let hits = check(
        &declared("animistic_verb"),
        "/// NodeId несёт поколение ячейки.\npub fn thing() {}\n",
    );
    assert_eq!(hits.len(), 1, "{hits:?}");
}

#[test]
fn named_operation_instead_of_a_metaphor_is_accepted() {
    let hits = check(
        &declared("animistic_verb"),
        "/// NodeId содержит поколение ячейки.\npub fn thing() {}\n",
    );
    assert!(hits.is_empty(), "{hits:?}");
}

#[test]
fn project_term_budit_is_accepted() {
    let hits = check(
        &declared("animistic_verb"),
        "/// Futex будит поток.\npub fn thing() {}\n",
    );
    assert!(hits.is_empty(), "{hits:?}");
}

#[test]
fn zavodit_verb_is_reported() {
    let hits = check(
        &declared("zavodit_verb"),
        "/// Компонент заводит сессию.\npub fn thing() {}\n",
    );
    assert_eq!(hits.len(), 1, "{hits:?}");
}

#[test]
fn named_operation_instead_of_zavodit_is_accepted() {
    let hits = check(
        &declared("zavodit_verb"),
        "/// Компонент создаёт сессию.\npub fn thing() {}\n",
    );
    assert!(hits.is_empty(), "{hits:?}");
}

#[test]
fn markdown_paragraph_joins_lines_separated_by_a_line_break_without_a_blank_line() {
    let hits = check_one(
        &declared("fronted_negation"),
        Path::new("docs/demo.md"),
        "Такого узла по пути\nнет.\n",
    );
    assert_eq!(hits.len(), 1, "{hits:?}");
}

#[test]
fn markdown_fenced_code_is_excluded_from_paragraph_text() {
    let hits = check_one(
        &declared("zavodit_verb"),
        Path::new("docs/demo.md"),
        "```rust\nКомпонент заводит сессию.\n```\n",
    );
    assert!(hits.is_empty(), "{hits:?}");
}

#[test]
fn a_verb_of_perception_is_reported() {
    let hits = check(
        &declared("animistic_verb"),
        "/// Наружу крейт смотрит модулем.\npub fn thing() {}\n",
    );
    assert_eq!(hits.len(), 1, "{hits:?}");
}

#[test]
fn a_verb_of_gait_is_reported() {
    let hits = check(
        &declared("animistic_verb"),
        "/// Типы через эту границу не ходят.\npub fn thing() {}\n",
    );
    assert_eq!(hits.len(), 1, "{hits:?}");
}

#[test]
fn the_operation_itself_is_accepted() {
    let hits = check(
        &declared("animistic_verb"),
        "/// Типы через эту границу не передаются.\npub fn thing() {}\n",
    );
    assert!(hits.is_empty(), "{hits:?}");
}

#[test]
fn a_board_name_in_shared_code_is_reported() {
    let hits = check(
        &declared("device_specific_name"),
        "fn lavender_windows_include_the_framebuffer() {}\n",
    );
    assert_eq!(hits.len(), 1, "{hits:?}");
}

#[test]
fn a_device_dtb_included_into_shared_code_is_reported() {
    let hits = check(
        &declared("device_specific_name"),
        "const DTB: &[u8] = include_bytes!(\"../../../devices/dtb/board.dtb\");\n",
    );
    assert_eq!(hits.len(), 1, "{hits:?}");
}

#[test]
fn a_generic_node_name_is_accepted() {
    let hits = check(
        &declared("device_specific_name"),
        "fn a_device_behind_the_soc_bus_has_a_window() {}\n",
    );
    assert!(hits.is_empty(), "{hits:?}");
}
