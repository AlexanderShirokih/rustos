use crate::{
    rule::Rule,
    rules::{
        comment_on_declaration, comment_on_test, comment_width, cyrillic_in_literal,
        doc_on_declaration,
    },
};

/// Перечень скомпилированных правил. Единственное место регистрации.
pub fn compiled() -> Vec<Rule> {
    vec![
        doc_on_declaration::rule(),
        comment_on_declaration::rule(),
        comment_on_test::rule(),
        cyrillic_in_literal::rule(),
        comment_width::rule(),
    ]
}
