use anyhow::{Context, Result, bail, ensure};

use super::{binding::ComponentBinding, source::ComponentEntry};

pub(super) fn resolve_after(
    component: &ComponentEntry,
    name: &str,
    names: &[&str],
) -> Result<Vec<usize>> {
    component
        .after
        .iter()
        .map(|edge| {
            ensure!(
                edge != name,
                "boot plan component '{name}' names itself in 'after'"
            );
            let index = names
                .iter()
                .position(|known| known == edge)
                .with_context(|| {
                    format!(
                        "boot plan component '{name}' names '{edge}' in 'after', \
                         which the composition does not contain"
                    )
                })?;
            Ok(index)
        })
        .collect()
}

pub(super) fn validate_stage_order(
    boundary: usize,
    bindings: &[ComponentBinding<'_>],
    after: &[Vec<usize>],
    names: &[&str],
) -> Result<()> {
    // Ступень 0 поднимается целиком до того, как её владелец политики загрузки подаст набор
    // ступени 1: назвать компонент ступени 1 её зависимость не может.
    let stage0 = bindings.iter().zip(after).zip(names).take(boundary);
    for ((binding, after), name) in stage0 {
        for &edge in binding.providers.iter().chain(after) {
            let target = names
                .get(edge)
                .context("boot plan names a dependency outside the composition")?;
            ensure!(
                edge < boundary,
                "boot plan component '{name}' depends on '{target}', which stage 1 raises"
            );
        }
    }
    Ok(())
}

pub(super) fn validate_acyclic(
    bindings: &[ComponentBinding<'_>],
    after: &[Vec<usize>],
    names: &[&str],
) -> Result<()> {
    // Порядок запуска - топологический по объединению провайдеров слотов и
    // after: цикл делает его невыразимым.
    let edges: Vec<Vec<usize>> = bindings
        .iter()
        .zip(after)
        .map(|(binding, after)| binding.providers.iter().chain(after).copied().collect())
        .collect();
    let outside = || "boot plan names a dependency outside the composition";
    let mut dependents: Vec<usize> = vec![0; names.len()];
    for &edge in edges.iter().flatten() {
        *dependents.get_mut(edge).with_context(outside)? += 1;
    }
    let mut ready: Vec<usize> = dependents
        .iter()
        .enumerate()
        .filter(|(_, count)| **count == 0)
        .map(|(index, _)| index)
        .collect();
    let mut ordered = 0;
    while let Some(index) = ready.pop() {
        ordered += 1;
        for &edge in edges.get(index).with_context(outside)? {
            let dependent = dependents.get_mut(edge).with_context(outside)?;
            *dependent -= 1;
            if *dependent == 0 {
                ready.push(edge);
            }
        }
    }
    if ordered != names.len() {
        let looped: Vec<&str> = dependents
            .iter()
            .enumerate()
            .filter(|(_, count)| **count != 0)
            .filter_map(|(index, _)| names.get(index).copied())
            .collect();
        bail!(
            "boot plan dependencies form a cycle over components: {}",
            looped.join(", ")
        );
    }
    Ok(())
}
