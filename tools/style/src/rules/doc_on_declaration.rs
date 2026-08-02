use crate::{
    rule::{Category, Check, Finding, Rule, RuleMeta, Severity},
    rules::declaration::{DECLARATIONS, carries_test_attribute, doc_block, permits_doc, word},
    scope::Applies,
    source::Source,
    view::{SyntaxView, View},
};

/// Правило B1: doc-комментарий стоит только над объявлением, объявленным `pub`.
pub fn rule() -> Rule {
    Rule {
        meta: RuleMeta {
            id: "doc_on_declaration".to_owned(),
            title: "doc comment on a declaration that is not declared `pub`".to_owned(),
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
            let Some(block) = doc_block(&node) else {
                continue;
            };
            if carries_test_attribute(&node, view.settings) || permits_doc(&node, view.settings) {
                continue;
            }
            out.push(Finding::new(
                block.span,
                format!(
                    "doc comment on {}, which is not declared `pub`",
                    word(node.kind())
                ),
            ));
        }
    }
}
