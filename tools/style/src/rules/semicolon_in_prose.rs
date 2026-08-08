use crate::{
    rule::{Category, Check, Finding, Rule, RuleMeta, Severity},
    rules::attached::{blocks, body_of},
    scope::{Applies, Context, MacroBodies, Scope},
    source::Source,
    span::Span,
    text::{has_cyrillic, mask_backticks},
};

/// Точка с запятой не разделяет русскую прозу, описывающую объявление.
pub fn rule() -> Rule {
    Rule {
        meta: RuleMeta {
            id: "semicolon_in_prose".to_owned(),
            title: "semicolon separates russian prose of a declaration".to_owned(),
            explain: concat!(
                "Russian prose parts with a full stop, a comma or a dash. A semicolon reads ",
                "as a list separator and hides where one statement ends and the next begins. ",
                "Split the sentence, or join the clauses with a comma. A semicolon closing an ",
                "item of a markdown list is left alone.",
            )
            .to_owned(),
            category: Category::Prose,
            severity: Severity::Warn,
        },
        applies: Applies {
            scope: Scope::rust(),
            context: Context::Any,
            macro_bodies: MacroBodies::Exclude,
        },
        check: Check::Compiled(run),
    }
}

fn run(source: &Source, out: &mut Vec<Finding>) {
    for block in blocks(source) {
        let mut fenced = false;
        let mut listed = false;
        for line in block {
            let (offset, body) = body_of(&line.text);
            let trimmed = body.trim_start();
            if trimmed.starts_with("```") {
                fenced = !fenced;
                continue;
            }
            if fenced {
                continue;
            }
            listed = if opens_item(trimmed) {
                true
            } else {
                listed && body.starts_with("  ")
            };
            let masked = mask_backticks(body);
            if !has_cyrillic(&masked) {
                continue;
            }
            for (at, ch) in masked.char_indices() {
                if ch != ';' {
                    continue;
                }
                if listed && masked[at + 1..].trim().is_empty() {
                    continue;
                }
                let start = line.span.start + (offset + at) as u32;
                out.push(Finding::new(
                    Span::new(start, start + 1),
                    "semicolon separates russian prose, use a full stop, a comma or a dash"
                        .to_owned(),
                ));
            }
        }
    }
}

fn opens_item(trimmed: &str) -> bool {
    let bullet = trimmed
        .strip_prefix('-')
        .or_else(|| trimmed.strip_prefix('*'))
        .or_else(|| trimmed.strip_prefix('+'));
    if let Some(rest) = bullet {
        return rest.starts_with(' ');
    }
    let digits: String = trimmed.chars().take_while(char::is_ascii_digit).collect();
    !digits.is_empty()
        && trimmed[digits.len()..]
            .strip_prefix('.')
            .is_some_and(|rest| rest.starts_with(' '))
}
