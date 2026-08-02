use std::{collections::BTreeSet, path::Path};

use style_tool::{Category, Config, Severity, compiled};

fn style_toml() -> String {
    std::fs::read_to_string(Path::new(env!("CARGO_MANIFEST_DIR")).join("../../style.toml"))
        .expect("read style.toml")
}

#[test]
fn rule_identifiers_are_unique() {
    let catalog = compiled();
    let Ok(mut config) = Config::parse(&style_toml(), &catalog) else {
        panic!("style.toml must load");
    };
    let full = config.catalog(compiled());
    let unique: BTreeSet<&str> = full.iter().map(|rule| rule.meta.id.as_str()).collect();
    assert_eq!(unique.len(), full.len());
}

#[test]
fn every_compiled_rule_has_a_source_file_named_after_it() {
    let directory = Path::new(env!("CARGO_MANIFEST_DIR")).join("src/rules");
    for rule in compiled() {
        let path = directory.join(format!("{}.rs", rule.meta.id));
        assert!(
            path.exists(),
            "missing source file for rule {}",
            rule.meta.id
        );
    }
}

#[test]
fn prose_rules_stay_at_warn() {
    let catalog = compiled();
    let Ok(mut config) = Config::parse(&style_toml(), &catalog) else {
        panic!("style.toml must load");
    };
    for rule in config.catalog(compiled()) {
        if rule.meta.category == Category::Prose {
            assert_eq!(rule.meta.severity, Severity::Warn, "{}", rule.meta.id);
        }
    }
}

#[test]
fn prose_rule_cannot_be_raised_to_deny() {
    let catalog = compiled();
    let raised = "[rules]\ncomment_width = \"deny\"\n";
    let Err(error) = Config::parse(raised, &catalog) else {
        panic!("prose ceiling must hold");
    };
    assert!(error.to_string().contains("ceiling"), "{error}");
}

#[test]
fn unknown_rule_in_configuration_is_rejected() {
    let catalog = compiled();
    let unknown = "[rules]\nno_such_rule = \"warn\"\n";
    let Err(error) = Config::parse(unknown, &catalog) else {
        panic!("unknown rule must be rejected");
    };
    assert!(error.to_string().contains("unknown rule id"), "{error}");
}

#[test]
fn broken_pattern_in_a_declared_rule_is_rejected() {
    let catalog = compiled();
    let broken = concat!(
        "[[text]]\n",
        "id = \"broken\"\n",
        "title = \"t\"\n",
        "message = \"m\"\n",
        "pattern = \"[unclosed\"\n"
    );
    let Err(error) = Config::parse(broken, &catalog) else {
        panic!("bad pattern must be rejected");
    };
    assert!(error.to_string().contains("compile pattern"), "{error}");
}

#[test]
fn declared_rule_may_not_shadow_a_compiled_one() {
    let catalog = compiled();
    let clash = concat!(
        "[[text]]\n",
        "id = \"comment_width\"\n",
        "title = \"t\"\n",
        "message = \"m\"\n",
        "pattern = \"x\"\n"
    );
    let Err(error) = Config::parse(clash, &catalog) else {
        panic!("duplicate id must be rejected");
    };
    assert!(error.to_string().contains("duplicate rule id"), "{error}");
}

#[test]
fn shipped_configuration_is_valid() {
    let catalog = compiled();
    assert!(Config::parse(&style_toml(), &catalog).is_ok());
}
