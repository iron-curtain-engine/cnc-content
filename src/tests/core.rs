// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (c) 2025–present Iron Curtain contributors

//! Integration tests for `GameId`, package definitions, and source invariants.
//!
//! Verifies that every enum variant has a matching table entry, slugs round-trip,
//! and required-package counts are sane across all seven supported games.

use super::super::*;

// ── GameId tests ────────────────────────────────────────────────────

/// Verifies that every `GameId` variant round-trips through its slug representation.
///
/// Guarantees that `slug()` and `from_slug()` are inverses for every variant in
/// `GameId::ALL`, so the slug-based CLI and config parsing cannot silently diverge
/// from the canonical enum values.
#[test]
fn game_id_slugs_roundtrip() {
    for &game in GameId::ALL {
        let slug = game.slug();
        let parsed = GameId::from_slug(slug).expect("slug should parse");
        assert_eq!(parsed, game);
    }
}

/// Verifies that all documented slug aliases resolve to the correct `GameId` variant.
///
/// Ensures user-facing short names ("ra", "td", "cnc", "d2k") and long names
/// ("redalert", "dune2000") all map correctly, and that an unknown slug returns
/// `None` rather than panicking or returning a wrong variant.
#[test]
fn game_id_from_slug_aliases() {
    assert_eq!(GameId::from_slug("ra"), Some(GameId::RedAlert));
    assert_eq!(GameId::from_slug("redalert"), Some(GameId::RedAlert));
    assert_eq!(GameId::from_slug("td"), Some(GameId::TiberianDawn));
    assert_eq!(GameId::from_slug("cnc"), Some(GameId::TiberianDawn));
    assert_eq!(GameId::from_slug("dune2"), Some(GameId::Dune2));
    assert_eq!(GameId::from_slug("dune2"), Some(GameId::Dune2));
    assert_eq!(GameId::from_slug("dune2000"), Some(GameId::Dune2000));
    assert_eq!(GameId::from_slug("d2k"), Some(GameId::Dune2000));
    assert_eq!(GameId::from_slug("ts"), Some(GameId::TiberianSun));
    assert_eq!(GameId::from_slug("tiberiansun"), Some(GameId::TiberianSun));
    assert_eq!(GameId::from_slug("ra2"), Some(GameId::RedAlert2));
    assert_eq!(GameId::from_slug("redalert2"), Some(GameId::RedAlert2));
    assert_eq!(GameId::from_slug("gen"), Some(GameId::Generals));
    assert_eq!(GameId::from_slug("generals"), Some(GameId::Generals));
    assert_eq!(GameId::from_slug("zh"), Some(GameId::Generals));
    assert_eq!(GameId::from_slug("unknown"), None);
}

// ── Red Alert package tests ─────────────────────────────────────────

/// Verifies that every Red Alert `PackageId` variant has a fully populated package definition.
///
/// Guards against accidentally adding a `PackageId` constant without a corresponding
/// entry in the package table, which would cause a panic at runtime when the package
/// is looked up.
#[test]
fn all_ra_package_ids_have_definitions() {
    let ids = [
        PackageId::RaBase,
        PackageId::RaAftermathBase,
        PackageId::RaCncDesert,
        PackageId::RaMusic,
        PackageId::RaMoviesAllied,
        PackageId::RaMoviesSoviet,
        PackageId::RaMusicCounterstrike,
        PackageId::RaMusicAftermath,
    ];
    for id in ids {
        let pkg = package(id).unwrap();
        assert_eq!(pkg.id, id);
        assert_eq!(pkg.game, GameId::RedAlert);
        assert!(!pkg.title.is_empty());
        assert!(!pkg.test_files.is_empty());
        assert!(!pkg.sources.is_empty());
    }
}

// ── Tiberian Dawn package tests ─────────────────────────────────────

/// Verifies that every Tiberian Dawn `PackageId` variant has a fully populated package definition.
///
/// Mirrors the RA equivalent: ensures all TD package constants resolve to a valid
/// package with a non-empty title and at least one test file, so install and verify
/// paths always have data to work with.
#[test]
fn all_td_package_ids_have_definitions() {
    let ids = [
        PackageId::TdBase,
        PackageId::TdCovertOps,
        PackageId::TdMusic,
        PackageId::TdMoviesGdi,
        PackageId::TdMoviesNod,
    ];
    for id in ids {
        let pkg = package(id).unwrap();
        assert_eq!(pkg.id, id);
        assert_eq!(pkg.game, GameId::TiberianDawn);
        assert!(!pkg.title.is_empty());
        assert!(!pkg.test_files.is_empty());
    }
}

// ── Dune 2 package tests ────────────────────────────────────────────

/// Verifies that the Dune 2 base package definition exists and is marked required.
///
/// Dune 2 has only one package; this ensures it is both present in the package table
/// and flagged `required`, so the engine will prompt for installation rather than
/// silently skipping it.
#[test]
fn dune2_package_has_definition() {
    let pkg = package(PackageId::Dune2Base).unwrap();
    assert_eq!(pkg.game, GameId::Dune2);
    assert!(pkg.required);
    assert!(!pkg.test_files.is_empty());
}

// ── Dune 2000 package tests ────────────────────────────────────────

/// Verifies that the Dune 2000 base package definition exists and is marked required.
///
/// Dune 2000 has only one package; this ensures it is present and `required`, matching
/// the same invariant as the Dune 2 package check.
#[test]
fn dune2000_package_has_definition() {
    let pkg = package(PackageId::Dune2000Base).unwrap();
    assert_eq!(pkg.game, GameId::Dune2000);
    assert!(pkg.required);
    assert!(!pkg.test_files.is_empty());
}

// ── Tiberian Sun package tests ──────────────────────────────────────

/// Verifies that every Tiberian Sun `PackageId` variant has a package definition.
///
/// Ensures all TS package constants resolve to a valid package with a non-empty
/// title and at least one test file and source.
#[test]
fn all_ts_package_ids_have_definitions() {
    let ids = [
        PackageId::TsBase,
        PackageId::TsFirestorm,
        PackageId::TsMusic,
        PackageId::TsMovies,
    ];
    for id in ids {
        let pkg = package(id).unwrap();
        assert_eq!(pkg.id, id);
        assert_eq!(pkg.game, GameId::TiberianSun);
        assert!(!pkg.title.is_empty());
        assert!(!pkg.test_files.is_empty());
        assert!(!pkg.sources.is_empty());
    }
}

// ── Red Alert 2 package tests ───────────────────────────────────────

/// Verifies that every Red Alert 2 `PackageId` variant has a package definition.
///
/// Ensures all RA2 package constants resolve to a valid package with a non-empty
/// title and at least one test file and source.
#[test]
fn all_ra2_package_ids_have_definitions() {
    let ids = [
        PackageId::Ra2Base,
        PackageId::Ra2YurisRevenge,
        PackageId::Ra2Music,
        PackageId::Ra2Movies,
    ];
    for id in ids {
        let pkg = package(id).unwrap();
        assert_eq!(pkg.id, id);
        assert_eq!(pkg.game, GameId::RedAlert2);
        assert!(!pkg.title.is_empty());
        assert!(!pkg.test_files.is_empty());
        assert!(!pkg.sources.is_empty());
    }
}

// ── Generals package tests ──────────────────────────────────────────

/// Verifies that every Generals `PackageId` variant has a package definition.
///
/// Ensures all Gen package constants resolve to a valid package with a non-empty
/// title and at least one test file and source.
#[test]
fn all_gen_package_ids_have_definitions() {
    let ids = [PackageId::GenBase, PackageId::GenZeroHour];
    for id in ids {
        let pkg = package(id).unwrap();
        assert_eq!(pkg.id, id);
        assert_eq!(pkg.game, GameId::Generals);
        assert!(!pkg.title.is_empty());
        assert!(!pkg.test_files.is_empty());
        assert!(!pkg.sources.is_empty());
    }
}

// ── Source tests ────────────────────────────────────────────────────

/// Verifies that every `SourceId` variant has a fully populated source definition.
///
/// Guarantees that all known installation sources (disc, Steam, Origin, GOG) are
/// registered in the source table with a non-empty title and at least one identity
/// file, so detection and extraction logic always has the metadata it needs.
#[test]
fn all_source_ids_have_definitions() {
    let ids = [
        // RA sources
        SourceId::AlliedDisc,
        SourceId::SovietDisc,
        SourceId::CounterstrikeDisc,
        SourceId::AftermathDisc,
        SourceId::TheFirstDecade,
        SourceId::Cnc95,
        SourceId::SteamTuc,
        SourceId::SteamCnc,
        SourceId::SteamRemastered,
        SourceId::OriginTuc,
        SourceId::OriginCnc,
        SourceId::OriginRemastered,
        // TD sources
        SourceId::TdGdiDisc,
        SourceId::TdNodDisc,
        SourceId::TdCovertOpsDisc,
        SourceId::TdSteamCnc,
        SourceId::TdSteamRemastered,
        SourceId::TdOriginCnc,
        // Dune 2 sources
        SourceId::Dune2Disc,
        SourceId::GogDune2,
        // Dune 2000 sources
        SourceId::Dune2000Disc,
        SourceId::GogDune2000,
        // TS sources
        SourceId::TsDisc,
        SourceId::TsFirestormDisc,
        SourceId::TsSteamTuc,
        SourceId::TsOriginTuc,
        // RA2 sources
        SourceId::Ra2Disc,
        SourceId::Ra2YrDisc,
        SourceId::Ra2TheFirstDecade,
        SourceId::Ra2SteamTuc,
        SourceId::Ra2OriginTuc,
        // Generals sources
        SourceId::GenDisc,
        SourceId::GenZhDisc,
        SourceId::GenSteamTuc,
        SourceId::GenOriginTuc,
    ];
    for id in ids {
        let src = source(id).unwrap();
        assert_eq!(src.id, id);
        assert!(!src.title.is_empty());
        assert!(!src.id_files.is_empty());
    }
}

// ── Required-package invariant tests ────────────────────────────────

/// Verifies that exactly one Tiberian Sun package is marked required: the base package.
///
/// TS expansions (Firestorm, music, movies) are optional; only the base data files
/// are needed to launch.
#[test]
fn ts_required_package_is_base() {
    let required: Vec<PackageId> = packages::ALL_PACKAGES
        .iter()
        .filter(|p| p.game == GameId::TiberianSun && p.required)
        .map(|p| p.id)
        .collect();
    assert_eq!(required, vec![PackageId::TsBase]);
}

/// Verifies that exactly one Red Alert 2 package is marked required: the base package.
///
/// RA2 expansions (Yuri's Revenge, music, movies) are optional; only the base data
/// files are needed to launch.
#[test]
fn ra2_required_package_is_base() {
    let required: Vec<PackageId> = packages::ALL_PACKAGES
        .iter()
        .filter(|p| p.game == GameId::RedAlert2 && p.required)
        .map(|p| p.id)
        .collect();
    assert_eq!(required, vec![PackageId::Ra2Base]);
}

/// Verifies that exactly one Generals package is marked required: the base package.
///
/// Zero Hour is optional; only the base game's BIG archives are needed to launch.
#[test]
fn gen_required_package_is_base() {
    let required: Vec<PackageId> = packages::ALL_PACKAGES
        .iter()
        .filter(|p| p.game == GameId::Generals && p.required)
        .map(|p| p.id)
        .collect();
    assert_eq!(required, vec![PackageId::GenBase]);
}
