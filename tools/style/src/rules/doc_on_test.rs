use crate::{
    rule::{Category, Check, Finding, Rule, RuleMeta, Severity},
    rules::declaration::{DECLARATIONS, carries_test_attribute, doc_block},
    scope::Applies,
    source::Source,
    view::{SyntaxView, View},
};

/// Правило B2: doc над тестовой функцией не длиннее порога regression guard.
pub fn rule() -> Rule {
    Rule {
        meta: RuleMeta {
            id: "doc_on_test".to_owned(),
            title: "doc comment on a test function exceeds the regression-guard budget".to_owned(),
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
    let budget = view.settings.test_doc_max_lines;
    for kind in DECLARATIONS {
        for node in view.nodes(*kind) {
            if !carries_test_attribute(&node, view.settings) {
                continue;
            }
            let Some(block) = doc_block(&node) else {
                continue;
            };
            if block.lines <= budget {
                continue;
            }
            out.push(Finding::new(
                block.span,
                format!(
                    "doc comment on a test spans {} lines, the regression-guard budget is {budget}",
                    block.lines
                ),
            ));
        }
    }
}
