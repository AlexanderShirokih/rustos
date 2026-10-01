use ra_ap_syntax::{
    AstNode, SyntaxKind, TextRange,
    ast::{self, HasAttrs, HasModuleItem, HasName},
};

use crate::{
    rule::{Category, Check, Finding, Rule, RuleMeta, Severity},
    scope::{Applies, Context, MacroBodies, Scope, Selector},
    source::Source,
    span::Span,
};

const PROC_MACRO_ATTRIBUTES: [&str; 3] =
    ["proc_macro", "proc_macro_attribute", "proc_macro_derive"];

/// Корневой `lib.rs` только объявляет подмодули и реэкспортирует их содержимое.
pub fn rule() -> Rule {
    Rule {
        meta: RuleMeta {
            id: "code_in_crate_root".to_owned(),
            title: "definition in the crate root".to_owned(),
            explain: concat!(
                "The crate root lists the modules of the crate and the API it exports: ",
                "`mod` declarations, `use` and `pub use`, `extern crate`. A definition, an ",
                "impl block or a macro invocation goes into a module of its own and reaches ",
                "the root through `pub use`. Procedural macro entry points stay in the root, ",
                "where the compiler requires them.",
            )
            .to_owned(),
            category: Category::Structure,
            severity: Severity::Deny,
        },
        applies: Applies {
            scope: Scope(vec![
                Selector::Extension("rs".to_owned()),
                Selector::FileNameAny(vec!["lib.rs".to_owned()]),
            ]),
            context: Context::Any,
            macro_bodies: MacroBodies::Include,
        },
        check: Check::Compiled(run),
    }
}

fn run(source: &Source, out: &mut Vec<Finding>) {
    let Some(file) = ast::SourceFile::cast(source.parsed().root.clone()) else {
        return;
    };
    for item in file.items() {
        if permitted(&item) {
            continue;
        }
        let range = anchor(&item);
        out.push(Finding::new(
            Span::new(range.start().into(), range.end().into()),
            "definition in the crate root, move it into a module and re-export it".to_owned(),
        ));
    }
}

fn permitted(item: &ast::Item) -> bool {
    match item {
        ast::Item::Use(_) | ast::Item::ExternCrate(_) => true,
        ast::Item::Module(module) => module.item_list().is_none(),
        ast::Item::Fn(function) => function.attrs().any(|attr| {
            attr.simple_name()
                .is_some_and(|name| PROC_MACRO_ATTRIBUTES.contains(&name.as_str()))
        }),
        _ => false,
    }
}

fn anchor(item: &ast::Item) -> TextRange {
    if let ast::Item::MacroCall(call) = item
        && let Some(path) = call.path()
    {
        return path.syntax().text_range();
    }
    if let Some(name) = ast::AnyHasName::cast(item.syntax().clone()).and_then(|owner| owner.name())
    {
        return name.syntax().text_range();
    }
    item.syntax()
        .children_with_tokens()
        .find(|element| {
            !matches!(
                element.kind(),
                SyntaxKind::ATTR | SyntaxKind::COMMENT | SyntaxKind::WHITESPACE
            )
        })
        .map_or_else(
            || item.syntax().text_range(),
            |element| element.text_range(),
        )
}
