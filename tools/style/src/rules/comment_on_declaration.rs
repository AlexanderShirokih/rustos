use crate::{
    rule::{Category, Check, Finding, Rule, RuleMeta, Severity},
    rules::declaration::{
        DECLARATIONS, carries_test_attribute, opens_safety_contract, permits_doc, plain_block, word,
    },
    scope::Applies,
    source::Source,
    view::{SyntaxView, View},
};

pub(crate) fn rule() -> Rule {
    Rule {
        meta: RuleMeta {
            id: "comment_on_declaration".to_owned(),
            title: "comment attached to a declaration that is not declared `pub`".to_owned(),
            explain: concat!(
                "Prose glued to a declaration documents it, whatever the marker: a comment ",
                "moved from `///` to `//` states the same contract in the same place. Outside ",
                "the public surface the name and the signature carry that contract, so the ",
                "comment goes away. An invariant the code cannot state stays as a `//` line at ",
                "the place it constrains, inside the body.",
            )
            .to_owned(),
            category: Category::Ast,
            severity: Severity::Deny,
        },
        applies: Applies::rust(),
        check: Check::Compiled(run),
    }
}

fn run(source: &Source, out: &mut Vec<Finding>) {
    let Some(view) = SyntaxView::of(source) else {
        return;
    };
    for kind in DECLARATIONS {
        for node in view.nodes(*kind) {
            let Some(block) = plain_block(&node) else {
                continue;
            };
            if carries_test_attribute(&node, view.settings) || permits_doc(&node, view.settings) {
                continue;
            }
            if opens_safety_contract(&node) {
                continue;
            }
            out.push(Finding::new(
                block.span,
                format!(
                    "comment attached to {}, which is not declared `pub`",
                    word(node.kind())
                ),
            ));
        }
    }
}
