use crate::{
    rule::{Category, Check, Finding, Rule, RuleMeta, Severity},
    rules::declaration::{DECLARATIONS, carries_test_attribute, leading_block},
    scope::Applies,
    source::Source,
    view::{SyntaxView, View},
};

pub(crate) fn rule() -> Rule {
    Rule {
        meta: RuleMeta {
            id: "comment_on_test".to_owned(),
            title: "comment on a test function exceeds the budget of a bug note".to_owned(),
            explain: concat!(
                "A test states its scenario through its name and its body. A comment above it ",
                "names the bug the test pins, in a line or two. ",
                "A retelling of the body goes away, and the marker of the comment does not ",
                "change the budget.",
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
    let budget = view.settings.test_comment_max_lines;
    for kind in DECLARATIONS {
        for node in view.nodes(*kind) {
            if !carries_test_attribute(&node, view.settings) {
                continue;
            }
            let Some(block) = leading_block(&node) else {
                continue;
            };
            if block.lines <= budget {
                continue;
            }
            out.push(Finding::new(
                block.span,
                format!(
                    "comment on a test spans {} lines, the budget of a bug note is {budget}",
                    block.lines
                ),
            ));
        }
    }
}
