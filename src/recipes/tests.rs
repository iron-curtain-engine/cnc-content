// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (c) 2025–present Iron Curtain contributors

//! Recipe module tests — coverage, invariant, and structural checks.
//!
//! Verifies that every source-package pair declared in the package
//! definitions has a corresponding install recipe, and that all recipes
//! reference valid data.

use super::ALL_RECIPES;
use crate::packages::ALL_PACKAGES;
use crate::sources::ALL_SOURCES;
use crate::{PackageId, SourceId};
use std::collections::HashSet;

// ── Structural invariants ───────────────────────────────────────────

/// Every recipe references a source ID that exists in ALL_SOURCES.
///
/// Guards against typos or stale enum variants — if a recipe references a
/// source that was removed, this test catches it at compile time via the
/// enum match, and at runtime via this lookup.
#[test]
fn all_recipe_sources_exist() {
    let known_sources: HashSet<SourceId> = ALL_SOURCES.iter().map(|s| s.id).collect();
    for recipe in ALL_RECIPES.iter() {
        assert!(
            known_sources.contains(&recipe.source),
            "recipe references unknown source {:?} for package {:?}",
            recipe.source,
            recipe.package
        );
    }
}

/// Every recipe references a package ID that exists in `ALL_PACKAGES`.
///
/// A recipe pointing to an unknown package ID would silently be skipped
/// by the executor, leaving required content uninstalled with no error.
#[test]
fn all_recipe_packages_exist() {
    let known_packages: HashSet<PackageId> = ALL_PACKAGES.iter().map(|p| p.id).collect();
    for recipe in ALL_RECIPES.iter() {
        assert!(
            known_packages.contains(&recipe.package),
            "recipe references unknown package {:?} from source {:?}",
            recipe.package,
            recipe.source
        );
    }
}

/// No duplicate recipes — each (source, package) pair must be unique.
///
/// Duplicate recipes would cause the executor to run extraction twice,
/// wasting I/O and potentially overwriting files mid-operation.
#[test]
fn no_duplicate_recipes() {
    let mut seen = HashSet::new();
    for recipe in ALL_RECIPES.iter() {
        let key = (recipe.source, recipe.package);
        assert!(
            seen.insert(key),
            "duplicate recipe: source {:?} × package {:?}",
            recipe.source,
            recipe.package
        );
    }
}

/// Every recipe has at least one action.
///
/// An empty action list is a no-op that wastes executor cycles and
/// misleads the user into thinking content was installed.
#[test]
fn all_recipes_have_actions() {
    for recipe in ALL_RECIPES.iter() {
        assert!(
            !recipe.actions.is_empty(),
            "recipe source {:?} × package {:?} has zero actions",
            recipe.source,
            recipe.package
        );
    }
}

// ── Coverage checks ─────────────────────────────────────────────────

/// Every source listed in a package's `sources` array has a recipe for that package.
///
/// If a package declares that it can be extracted from a given source, the
/// recipe system must have a matching recipe. Missing recipes mean the
/// executor cannot install content from a detected source, even though the
/// package metadata says it should be possible.
#[test]
fn every_package_source_has_recipe() {
    let recipe_set: HashSet<(SourceId, PackageId)> =
        ALL_RECIPES.iter().map(|r| (r.source, r.package)).collect();

    let mut missing = Vec::new();
    for pkg in ALL_PACKAGES {
        for &src in pkg.sources {
            if !recipe_set.contains(&(src, pkg.id)) {
                missing.push((src, pkg.id));
            }
        }
    }

    assert!(
        missing.is_empty(),
        "missing recipes for {} source×package pairs:\n{}",
        missing.len(),
        missing
            .iter()
            .map(|(s, p)| format!("  {:?} × {:?}", s, p))
            .collect::<Vec<_>>()
            .join("\n")
    );
}

// ── Per-game recipe counts ──────────────────────────────────────────

/// Red Alert has recipes covering all its source-package combinations.
///
/// The minimum count acts as a regression guard — silently deleting or
/// merging recipe files would reduce the count below the threshold and
/// fail here before any content becomes uninstallable at runtime.
#[test]
fn ra_recipe_count() {
    let ra_recipes: Vec<_> = ALL_RECIPES
        .iter()
        .filter(|r| {
            ALL_PACKAGES
                .iter()
                .any(|p| p.id == r.package && p.game == crate::GameId::RedAlert)
        })
        .collect();
    // At minimum: 10 sources × several packages each.
    assert!(
        ra_recipes.len() >= 20,
        "expected ≥20 RA recipes, got {}",
        ra_recipes.len()
    );
}

/// Tiberian Dawn has recipes for all its sources.
///
/// Same regression-guard rationale as `ra_recipe_count`: the floor catches
/// accidental recipe loss before it reaches users.
#[test]
fn td_recipe_count() {
    let td_recipes: Vec<_> = ALL_RECIPES
        .iter()
        .filter(|r| {
            ALL_PACKAGES
                .iter()
                .any(|p| p.id == r.package && p.game == crate::GameId::TiberianDawn)
        })
        .collect();
    // 6 sources, 5 packages, not all combinations exist.
    assert!(
        td_recipes.len() >= 15,
        "expected ≥15 TD recipes, got {}",
        td_recipes.len()
    );
}

/// Tiberian Sun has recipes for all its sources.
///
/// Same regression-guard rationale as `ra_recipe_count`: the floor catches
/// accidental recipe loss before it reaches users.
#[test]
fn ts_recipe_count() {
    let ts_recipes: Vec<_> = ALL_RECIPES
        .iter()
        .filter(|r| {
            ALL_PACKAGES
                .iter()
                .any(|p| p.id == r.package && p.game == crate::GameId::TiberianSun)
        })
        .collect();
    assert!(
        ts_recipes.len() >= 10,
        "expected ≥10 TS recipes, got {}",
        ts_recipes.len()
    );
}

/// Dune 2 and Dune 2000 each have recipes for disc and GOG sources.
///
/// These are local-source-only games with a fixed, small source set; an
/// exact count (not a floor) is appropriate because every combination
/// must be covered and none should be silently added.
#[test]
fn dune_recipe_count() {
    let dune_recipes: Vec<_> = ALL_RECIPES
        .iter()
        .filter(|r| {
            ALL_PACKAGES.iter().any(|p| {
                p.id == r.package
                    && (p.game == crate::GameId::Dune2 || p.game == crate::GameId::Dune2000)
            })
        })
        .collect();
    // 2 games × 2 sources each = 4 recipes minimum.
    assert_eq!(dune_recipes.len(), 4, "expected 4 Dune recipes");
}

/// RA2 and Generals have recipes for all their sources.
///
/// Same regression-guard rationale as `ra_recipe_count`: the floor catches
/// accidental recipe loss before it reaches users.
#[test]
fn ra2_gen_recipe_count() {
    let ra2_gen_recipes: Vec<_> = ALL_RECIPES
        .iter()
        .filter(|r| {
            ALL_PACKAGES.iter().any(|p| {
                p.id == r.package
                    && (p.game == crate::GameId::RedAlert2 || p.game == crate::GameId::Generals)
            })
        })
        .collect();
    // RA2: 5 sources × 4 packages (not all combos), Gen: 3 sources × 2 packages.
    assert!(
        ra2_gen_recipes.len() >= 15,
        "expected ≥15 RA2+Gen recipes, got {}",
        ra2_gen_recipes.len()
    );
}
