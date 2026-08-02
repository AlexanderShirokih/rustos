use crate::{
    rule::{Finding, Strip, TextRule, TextTarget},
    source::Source,
    span::Span,
    text::mask_backticks,
    view::{CommentView, LineView, View},
};

/// Исполняет правило, объявленное данными, над единицами текста файла.
pub fn run(rule: &TextRule, source: &Source, out: &mut Vec<Finding>) {
    for unit in units(rule.target, source) {
        let subject = strip(&unit.text, &rule.strip);
        if rule.unless.iter().any(|veto| veto.is_match(&subject)) {
            continue;
        }
        if !rule.require.iter().all(|need| need.is_match(&subject)) {
            continue;
        }
        for hit in rule.pattern.find_iter(&subject) {
            out.push(Finding::new(
                Span::new(
                    unit.start + hit.start() as u32,
                    unit.start + hit.end() as u32,
                ),
                rule.message.clone(),
            ));
        }
    }
}

struct Unit {
    start: u32,
    text: String,
}

fn units(target: TextTarget, source: &Source) -> Vec<Unit> {
    match target {
        TextTarget::Comment | TextTarget::Doc => {
            let Some(view) = CommentView::of(source) else {
                return Vec::new();
            };
            view.comments
                .iter()
                .filter(|comment| target != TextTarget::Doc || comment.is_doc)
                .map(|comment| Unit {
                    start: comment.span.start,
                    text: comment.text.clone(),
                })
                .collect()
        }
        TextTarget::Line => {
            let Some(view) = LineView::of(source) else {
                return Vec::new();
            };
            (0..view.count())
                .map(|number| Unit {
                    start: view.span(number).start,
                    text: view.text_of(number).to_owned(),
                })
                .collect()
        }
    }
}

fn strip(text: &str, steps: &[Strip]) -> String {
    let mut current = text.to_owned();
    for step in steps {
        current = match step {
            Strip::Backticks => mask_backticks(&current),
        };
    }
    current
}
