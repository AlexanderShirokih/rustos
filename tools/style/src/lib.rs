//! Статический валидатор конвенций кода: doc-дисциплина, язык литералов, проза комментариев.

mod anchor;
mod baseline;
mod changeset;
mod config;
mod engine;
mod report;
mod rule;
mod rules;
mod scope;
mod settings;
mod source;
mod span;
mod suppress;
mod testkit;
mod text;
mod view;
mod workspace;

pub use crate::{
    baseline::Baseline,
    config::Config,
    engine::{Engine, Outcome, Run, Selection},
    report::{Format, Report},
    rule::{Category, Finding, Rule, RuleMeta, Severity},
    rules::catalog::compiled,
    scope::{Applies, Context, Domain, FileMeta, MacroBodies, Scope, Selector},
    settings::Settings,
    source::Source,
    span::{LineIndex, Position, Span},
    testkit::check_one,
};
