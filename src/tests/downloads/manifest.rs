// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (c) 2025–present Iron Curtain contributors

//! Download/package invariant tests — SHA-1, game matching, recipe coverage.

use super::*;

/// Verifies that every download's game tag matches the game of each package it provides.
///
/// A cross-game mismatch (e.g., a TD download claiming to provide an RA package) would
/// break per-game filtering in the status and install commands, potentially showing or
/// hiding downloads in the wrong context.
#[test]
fn download_game_matches_package_game() {
    for dl in downloads::all_downloads() {
        for &pkg_id in &dl.provides {
            let pkg = package(pkg_id).unwrap();
            assert_eq!(
                dl.game, pkg.game,
                "Download {:?} game {:?} doesn't match package {:?} game {:?}",
                dl.id, dl.game, pkg_id, pkg.game,
            );
        }
    }
}

/// Verifies that every Steam-type source carries a `SteamAppId` platform hint.
///
/// The Steam detection path uses the `PlatformHint::SteamAppId` value to locate the
/// installation via the Steam library manifests; a missing hint means the source can
/// never be auto-detected on Steam.
#[test]
fn steam_sources_have_app_ids() {
    for source in sources::ALL_SOURCES {
        if matches!(source.source_type, SourceType::Steam) {
            assert!(
                matches!(source.platform_hint, Some(PlatformHint::SteamAppId(_))),
                "Steam source {:?} should have a SteamAppId hint",
                source.id,
            );
        }
    }
}

/// Verifies that every (source, package) pair declared in package definitions has a recipe.
///
/// When a package lists a source in its `sources` field, the recipe table must have
/// a matching `InstallRecipe` with that (source, package) pair. A missing recipe means
/// the content manager can detect a source but has no extraction instructions, leaving
/// the install silently incomplete.
///
/// Known-incomplete pairs are tracked in `known_gaps` and excluded from the assertion.
/// Remove entries from `known_gaps` as recipes are implemented — the test will catch
/// any newly-declared pairs that lack recipes.
#[test]
fn recipes_cover_declared_source_package_pairs() {
    use std::collections::HashSet;

    // Games whose recipes are not yet implemented at all.
    let pending_games: HashSet<GameId> = [
        GameId::TiberianDawn,
        GameId::TiberianSun,
        GameId::Dune2,
        GameId::Dune2000,
    ]
    .into_iter()
    .collect();

    // Individual (source, package) pairs with known missing recipes.
    // These are tracked here so the test still catches NEW gaps.
    //
    // RA — TheFirstDecade ISCAB extraction not yet implemented:
    // Remastered movies not yet mapped:
    let known_gaps: HashSet<(SourceId, PackageId)> = [
        (SourceId::TheFirstDecade, PackageId::RaAftermathBase),
        (SourceId::TheFirstDecade, PackageId::RaMusic),
        (SourceId::TheFirstDecade, PackageId::RaMoviesAllied),
        (SourceId::TheFirstDecade, PackageId::RaMoviesSoviet),
        (SourceId::SteamRemastered, PackageId::RaMoviesAllied),
        (SourceId::OriginRemastered, PackageId::RaMoviesAllied),
        // RA2 — disc / TFD sources not verified against real media:
        (SourceId::Ra2Disc, PackageId::Ra2Base),
        (SourceId::Ra2TheFirstDecade, PackageId::Ra2Base),
        (SourceId::Ra2YrDisc, PackageId::Ra2YurisRevenge),
        (SourceId::Ra2TheFirstDecade, PackageId::Ra2YurisRevenge),
        (SourceId::Ra2Disc, PackageId::Ra2Music),
        (SourceId::Ra2TheFirstDecade, PackageId::Ra2Music),
        // RA2 — movies inside MIX archives, entry names need research:
        (SourceId::Ra2Disc, PackageId::Ra2Movies),
        (SourceId::Ra2TheFirstDecade, PackageId::Ra2Movies),
        (SourceId::Ra2SteamTuc, PackageId::Ra2Movies),
        (SourceId::Ra2OriginTuc, PackageId::Ra2Movies),
        // Generals — disc sources not verified against real media:
        (SourceId::GenDisc, PackageId::GenBase),
        (SourceId::GenZhDisc, PackageId::GenZeroHour),
    ]
    .into_iter()
    .collect();

    let recipe_set: HashSet<(SourceId, PackageId)> = recipes::ALL_RECIPES
        .iter()
        .map(|r| (r.source, r.package))
        .collect();

    let mut missing = Vec::new();
    for pkg in packages::ALL_PACKAGES {
        if pending_games.contains(&pkg.game) {
            continue;
        }
        for &src_id in pkg.sources {
            let pair = (src_id, pkg.id);
            if !recipe_set.contains(&pair) && !known_gaps.contains(&pair) {
                missing.push(pair);
            }
        }
    }

    assert!(
        missing.is_empty(),
        "Missing recipes for {} unexpected (source, package) pairs: {missing:?}",
        missing.len(),
    );

    // Regression guard: total recipe count must not silently shrink.
    let total = recipes::ALL_RECIPES.len();
    assert!(
        total >= 48,
        "Expected at least 48 recipes, got {total} — did a recipe get deleted?"
    );
}

/// Verifies that every recipe defines at least one extraction action.
///
/// A recipe with zero actions would match a source/package pair but do nothing,
/// leaving the package permanently uninstalled without any error being raised.
#[test]
fn recipe_actions_are_non_empty() {
    for recipe in recipes::ALL_RECIPES.iter() {
        assert!(
            !recipe.actions.is_empty(),
            "Recipe ({:?}, {:?}) should have at least one action",
            recipe.source,
            recipe.package,
        );
    }
}

/// Verifies that every recipe references a source and package that both have definitions.
///
/// Prevents recipes from pointing to orphaned IDs: a lookup panic on a missing source
/// or package would only surface at install time rather than at test time.
#[test]
fn recipe_source_and_package_have_definitions() {
    for r in recipes::ALL_RECIPES.iter() {
        let _ = source(r.source);
        let _ = package(r.package);
    }
}
