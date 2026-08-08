use ra_ap_syntax::{AstNode, SyntaxKind, SyntaxNode, ast};

use crate::{
    rule::{Category, Check, Finding, Rule, RuleMeta, Severity},
    rules::declaration::permits_doc,
    scope::{Applies, Context, MacroBodies, Scope, Selector},
    source::Source,
    span::Span,
    view::{SyntaxView, View},
};

const TEST_SEGMENTS: [&str; 3] = ["tests", "kernel_tests", "benches"];
const TEST_FILES: [&str; 2] = ["build.rs", "test_util.rs"];
const TEST_PACKAGES: [&str; 2] = ["ipc-test", "testrunner"];

/// Публичная функция не принимает и не возвращает кортеж.
pub fn rule() -> Rule {
    Rule {
        meta: RuleMeta {
            id: "tuple_in_public_signature".to_owned(),
            title: "tuple in the signature of a public function".to_owned(),
            explain: concat!(
                "A tuple names its parts by position, and the caller of a public API has ",
                "nothing but the order to go by. Several values leave through a named struct ",
                "whose fields carry the names.",
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
                Selector::NotPackageAny(TEST_PACKAGES.map(str::to_owned).to_vec()),
            ]),
            context: Context::Prod,
            macro_bodies: MacroBodies::Exclude,
        },
        check: Check::Compiled(run),
    }
}

fn run(source: &Source, out: &mut Vec<Finding>) {
    let Some(view) = SyntaxView::of(source) else {
        return;
    };
    for node in view.nodes(SyntaxKind::FN) {
        if !permits_doc(&node, view.settings) {
            continue;
        }
        let Some(declaration) = ast::Fn::cast(node) else {
            continue;
        };
        let parts = declaration
            .param_list()
            .map(|list| list.syntax().clone())
            .into_iter()
            .chain(declaration.ret_type().map(|ret| ret.syntax().clone()));
        for part in parts {
            for tuple in tuples(&part) {
                let range = tuple.syntax().text_range();
                out.push(Finding::new(
                    Span::new(range.start().into(), range.end().into()),
                    "tuple in the signature of a public function, name the parts with a struct"
                        .to_owned(),
                ));
            }
        }
    }
}

fn tuples(part: &SyntaxNode) -> impl Iterator<Item = ast::TupleType> {
    part.descendants()
        .filter_map(ast::TupleType::cast)
        .filter(|tuple| tuple.fields().next().is_some())
}
