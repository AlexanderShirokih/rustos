use crate::{
    rule::{Category, Check, Finding, Rule, RuleMeta, Severity},
    scope::{Applies, Context, MacroBodies, Scope},
    source::Source,
    text::width,
    view::{CommentView, LineView, View},
};

pub(crate) fn rule() -> Rule {
    Rule {
        meta: RuleMeta {
            id: "comment_width".to_owned(),
            title: "comment line is wider than the code width limit".to_owned(),
            explain: concat!(
                "A comment wraps at the width `rustfmt.toml` sets for code, so a block of ",
                "prose keeps the shape of the file around it. Rewrap the block at that width.",
            )
            .to_owned(),
            category: Category::Prose,
            severity: Severity::Warn,
        },
        applies: Applies {
            scope: Scope::rust(),
            context: Context::Any,
            macro_bodies: MacroBodies::Include,
        },
        check: Check::Compiled(run),
    }
}

fn run(source: &Source, out: &mut Vec<Finding>) {
    let Some((lines, comments)) = <(LineView, CommentView)>::of(source) else {
        return;
    };
    let limit = lines.settings.max_width;
    let mut reported: Vec<usize> = Vec::new();

    for comment in comments.comments {
        let first = lines.lines.line_of(comment.span.start);
        let last = lines.lines.line_of(comment.span.end.saturating_sub(1));
        for number in first..=last {
            if reported.contains(&number) {
                continue;
            }
            let span = lines.span(number);
            let text = lines.text_of(number);
            if width(text) <= limit {
                continue;
            }
            reported.push(number);
            out.push(Finding::new(
                span,
                format!(
                    "line carrying a comment is {} characters wide, the limit is {limit}",
                    width(text)
                ),
            ));
        }
    }
}
