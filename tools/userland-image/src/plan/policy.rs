use anyhow::{Context, Result, bail, ensure};
use syscall::Timeout;
use userland_image::OnExhausted;

use super::source::ComponentEntry;
use crate::units::{parse_duration, parse_memory_pages};

const DEFAULT_STARTUP_TIMEOUT: Timeout = Timeout::from_secs(2);

const RESTART_KEYWORD: &str = "restart";
const THEN_KEYWORD: &str = "then";
const OUTCOME_HALT: &str = "halt";
const OUTCOME_DETACH: &str = "detach";

struct FailurePolicy {
    launches: u32,
    on_exhausted: OnExhausted,
}

pub(super) struct Policy {
    pub(super) memory_pages: u64,
    pub(super) startup_timeout: Timeout,
    pub(super) launches: u32,
    pub(super) on_exhausted: OnExhausted,
}

pub(super) fn compile_policy(component: &ComponentEntry, name: &str) -> Result<Policy> {
    let memory_pages = parse_memory_pages(&component.memory_limit)
        .with_context(|| format!("boot plan component '{name}' sets 'memory_limit'"))?;
    let startup_timeout = match &component.startup_timeout {
        Some(text) => parse_duration(text)
            .with_context(|| format!("boot plan component '{name}' sets 'startup_timeout'"))?,
        None => DEFAULT_STARTUP_TIMEOUT,
    };
    let policy = parse_failure_policy(&component.on_failure)
        .with_context(|| format!("boot plan component '{name}' sets 'on_failure'"))?;
    Ok(Policy {
        memory_pages,
        startup_timeout,
        launches: policy.launches,
        on_exhausted: policy.on_exhausted,
    })
}

fn parse_failure_policy(text: &str) -> Result<FailurePolicy> {
    let tokens: Vec<&str> = text.split_whitespace().collect();
    let (restarts, outcome) = match tokens.as_slice() {
        [outcome] => (0, *outcome),
        [RESTART_KEYWORD, count, THEN_KEYWORD, outcome] => (parse_restarts(count)?, *outcome),
        _ => bail!(
            "'{text}' is not '{OUTCOME_HALT}', '{OUTCOME_DETACH}' \
             or '{RESTART_KEYWORD} N {THEN_KEYWORD} {OUTCOME_HALT}|{OUTCOME_DETACH}'"
        ),
    };
    let on_exhausted = match outcome {
        OUTCOME_HALT => OnExhausted::Halt,
        OUTCOME_DETACH => OnExhausted::Detach,
        _ => bail!("'{outcome}' is not '{OUTCOME_HALT}' or '{OUTCOME_DETACH}'"),
    };
    // Первый запуск перезапуском не считается: он есть у любого компонента.
    let launches = restarts
        .checked_add(1)
        .with_context(|| format!("restart count {restarts} overflows"))?;
    Ok(FailurePolicy {
        launches,
        on_exhausted,
    })
}

fn parse_restarts(text: &str) -> Result<u32> {
    let count: u32 = text
        .parse()
        .with_context(|| format!("'{text}' is not a restart count"))?;
    ensure!(
        count != 0,
        "'{RESTART_KEYWORD} 0' carries no restarts: name the outcome alone"
    );
    Ok(count)
}
