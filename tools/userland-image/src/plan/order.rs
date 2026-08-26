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
    for index in 0..boundary {
        for &edge in bindings[index].providers.iter().chain(&after[index]) {
            ensure!(
                edge < boundary,
                "boot plan component '{}' depends on '{}', which stage 1 raises",
                names[index],
                names[edge]
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
    let edges = |index: usize| bindings[index].providers.iter().chain(&after[index]);
    let mut dependents: Vec<usize> = vec![0; names.len()];
    for index in 0..names.len() {
        for &edge in edges(index) {
            dependents[edge] += 1;
        }
    }
    let mut ready: Vec<usize> = (0..names.len())
        .filter(|index| dependents[*index] == 0)
        .collect();
    let mut ordered = 0;
    while let Some(index) = ready.pop() {
        ordered += 1;
        for &edge in edges(index) {
            dependents[edge] -= 1;
            if dependents[edge] == 0 {
                ready.push(edge);
            }
        }
    }
    if ordered != names.len() {
        let looped: Vec<&str> = (0..names.len())
            .filter(|index| dependents[*index] != 0)
            .map(|index| names[index])
            .collect();
        bail!(
            "boot plan dependencies form a cycle over components: {}",
            looped.join(", ")
        );
    }
    Ok(())
}
