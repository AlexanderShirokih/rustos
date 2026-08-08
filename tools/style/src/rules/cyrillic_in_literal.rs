use crate::{
    rule::{Category, Check, Finding, Rule, RuleMeta, Severity},
    scope::{Applies, Context, MacroBodies, Scope, Selector},
    source::Source,
    text::has_cyrillic,
    view::{LiteralView, View},
};

const TEST_SEGMENTS: [&str; 3] = ["tests", "kernel_tests", "benches"];
const TEST_FILES: [&str; 1] = ["test_util.rs"];

/// Строковый литерал не содержит кириллицы, включая тела макро-вызовов.
pub fn rule() -> Rule {
    Rule {
        meta: RuleMeta {
            id: "cyrillic_in_literal".to_owned(),
            title: "cyrillic text in a string literal reaches program output".to_owned(),
            explain: concat!(
                "A string literal reaches the user through output, a panic or a log, and ",
                "that surface is english. Translate the literal and keep the russian text ",
                "in comments and documentation.",
            )
            .to_owned(),
            category: Category::Ast,
            severity: Severity::Deny,
        },
        applies: Applies {
            scope: Scope(vec![
                Selector::Extension("rs".to_owned()),
                Selector::NotSegmentAny(TEST_SEGMENTS.map(str::to_owned).to_vec()),
                Selector::NotFileNameAny(TEST_FILES.map(str::to_owned).to_vec()),
            ]),
            context: Context::Prod,
            macro_bodies: MacroBodies::Include,
        },
        check: Check::Compiled(run),
    }
}

fn run(source: &Source, out: &mut Vec<Finding>) {
    let Some(view) = LiteralView::of(source) else {
        return;
    };
    for literal in view.literals {
        if !has_cyrillic(&literal.text) {
            continue;
        }
        out.push(Finding::new(
            literal.span,
            "cyrillic text in a string literal, program output is english".to_owned(),
        ));
    }
}
