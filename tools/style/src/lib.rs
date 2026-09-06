//! Проверяет конвенции, которые не берут `cargo fmt` и `cargo clippy`: дисциплину
//! doc-комментариев, язык строковых литералов, формулировки прозы.
//!
//! [`Engine::run`] принимает [`Run`] с корнем поиска репозитория и настройками прогона, а
//! возвращает [`Outcome`]: находки, строки подвала и код возврата. Набор правил собирает
//! [`Config::parse`] из `style.toml` поверх скомпилированного каталога [`compiled`].
//! Известные находки гасит [`Baseline`], печатает результат [`Outcome::render`].
//!
//! Крейт работает на host. Прогон запускает `cargo xtask style`.

#![allow(clippy::std_instead_of_core)]

mod anchor;
mod baseline;
mod changeset;
mod config;
mod engine;
mod growth;
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
    growth::{Comparison, Grown, NewRule, grown_against},
    report::{Format, Report},
    rule::{Category, Finding, Rule, RuleMeta, Severity},
    rules::catalog::compiled,
    scope::{Applies, Context, Domain, FileMeta, MacroBodies, Scope, Selector},
    settings::Settings,
    source::Source,
    span::{LineIndex, Position, Span},
    testkit::check_one,
};
