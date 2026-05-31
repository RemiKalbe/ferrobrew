//! Runtime dependency resolution and install ordering.
//!
//! Installing from a bottle only needs *runtime* dependencies (build dependencies are irrelevant to
//! an already-compiled bottle). This resolves the transitive runtime-dependency graph of a formula
//! and returns the formulae in install order — every dependency before the formula that needs it.
//!
//! The full Homebrew nuance (`uses_from_macos` OS-version bounds, optional/recommended handling) is
//! a planned refinement tracked in `specs/deps.md`; v1 uses the `dependencies` list from the API.

use std::collections::HashSet;

use crate::error::Result;
use crate::formula::RawFormula;

/// Resolve `root` and its transitive runtime dependencies into install order (dependencies first,
/// `root` last), de-duplicated.
///
/// `fetch` loads a formula by name (e.g. via the JSON API); it is injected so resolution can be
/// unit-tested without network access.
pub fn resolve_install_order<F>(root: &str, mut fetch: F) -> Result<Vec<RawFormula>>
where
    F: FnMut(&str) -> Result<RawFormula>,
{
    let mut ordered = Vec::new();
    let mut visited = HashSet::new();
    let mut in_progress = HashSet::new();
    visit(
        root,
        &mut fetch,
        &mut ordered,
        &mut visited,
        &mut in_progress,
    )?;
    Ok(ordered)
}

fn visit<F>(
    name: &str,
    fetch: &mut F,
    ordered: &mut Vec<RawFormula>,
    visited: &mut HashSet<String>,
    in_progress: &mut HashSet<String>,
) -> Result<()>
where
    F: FnMut(&str) -> Result<RawFormula>,
{
    if visited.contains(name) {
        return Ok(());
    }
    // Homebrew's runtime graph is acyclic; guard anyway so a bad graph can't loop forever.
    if !in_progress.insert(name.to_string()) {
        return Ok(());
    }

    let formula = fetch(name)?;
    let dependencies = formula.dependencies.clone();
    for dependency in &dependencies {
        visit(dependency, fetch, ordered, visited, in_progress)?;
    }

    in_progress.remove(name);
    visited.insert(name.to_string());
    ordered.push(formula);
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::error::FerroError;
    use std::collections::HashMap;

    fn formula(name: &str, deps: &[&str]) -> RawFormula {
        RawFormula {
            name: name.to_string(),
            dependencies: deps.iter().map(|d| d.to_string()).collect(),
            ..Default::default()
        }
    }

    fn diamond() -> HashMap<String, RawFormula> {
        // root → a, b ; a → c ; b → c ; c → ∅
        [
            ("root", vec!["a", "b"]),
            ("a", vec!["c"]),
            ("b", vec!["c"]),
            ("c", vec![]),
        ]
        .into_iter()
        .map(|(n, d)| (n.to_string(), formula(n, &d)))
        .collect()
    }

    #[test]
    fn resolves_deps_before_dependents_and_dedups() {
        let graph = diamond();
        let order = resolve_install_order("root", |name| {
            graph
                .get(name)
                .cloned()
                .ok_or_else(|| FerroError::NotFound(name.to_string()))
        })
        .unwrap();
        let names: Vec<&str> = order.iter().map(|f| f.name.as_str()).collect();

        let pos = |n: &str| names.iter().position(|x| *x == n).unwrap();
        assert_eq!(names.last(), Some(&"root"));
        assert!(pos("c") < pos("a") && pos("c") < pos("b"));
        assert!(pos("a") < pos("root") && pos("b") < pos("root"));
        assert_eq!(
            names.iter().filter(|n| **n == "c").count(),
            1,
            "c installed once"
        );
    }

    #[test]
    fn propagates_fetch_errors() {
        let result = resolve_install_order("missing", |name| {
            Err(FerroError::NotFound(name.to_string()))
        });
        assert!(result.is_err());
    }
}
