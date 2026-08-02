use crate::{
    rule::Rule,
    rules::{comment_width, cyrillic_in_literal, doc_on_declaration, doc_on_test},
};

/// Перечень скомпилированных правил. Единственное место регистрации.
pub fn compiled() -> Vec<Rule> {
    vec![
        doc_on_declaration::rule(),
        doc_on_test::rule(),
        cyrillic_in_literal::rule(),
        comment_width::rule(),
    ]
}
