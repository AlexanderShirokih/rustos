use ra_ap_syntax::{AstNode, SyntaxKind, ast};

use crate::{
    rule::{Category, Check, Finding, Rule, RuleMeta, Severity},
    scope::{Applies, Context, Domain, MacroBodies, Scope, Selector},
    source::Source,
    span::Span,
    view::{SyntaxView, View},
};

const TEST_SEGMENTS: [&str; 3] = ["tests", "kernel_tests", "benches"];
const TEST_FILES: [&str; 2] = ["build.rs", "test_util.rs"];

/// Код ядра не срывается в панику: `expect` на достижимом пути запрещён.
pub fn rule() -> Rule {
    Rule {
        meta: RuleMeta {
            id: "expect_in_kernel".to_owned(),
            title: "kernel code reaches a panic through `expect`".to_owned(),
            explain: concat!(
                "A panic in the kernel takes the whole machine down, so a reachable path ",
                "never has one. Return the failure through `Result`, or make the impossible ",
                "state unrepresentable with `NonZero`, a newtype or a closed enum.",
            )
            .to_owned(),
            category: Category::Ast,
            severity: Severity::Deny,
        },
        applies: Applies {
            scope: Scope(vec![
                Selector::Extension("rs".to_owned()),
                Selector::DomainAny(vec![Domain::Kernel]),
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
    let Some(view) = SyntaxView::of(source) else {
        return;
    };
    for node in view.nodes(SyntaxKind::METHOD_CALL_EXPR) {
        let Some(call) = ast::MethodCallExpr::cast(node) else {
            continue;
        };
        let Some(name) = call.name_ref() else {
            continue;
        };
        if name.text() != "expect" {
            continue;
        }
        let range = name.syntax().text_range();
        out.push(Finding::new(
            Span::new(range.start().into(), range.end().into()),
            "expect in kernel code, return the failure through Result".to_owned(),
        ));
    }
}
