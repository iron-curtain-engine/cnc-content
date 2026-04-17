// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (c) 2025–present Iron Curtain contributors

//! Integration tests for download resolution, mirror lists, and seeding policy.
//!
//! Verifies that every `DownloadId` has a populated definition, mirror URLs
//! are well-formed, and seeding-policy selection logic is correct.

use super::super::*;

mod content;
mod manifest;
mod mirrors;
mod seeding;
mod torrents;

// ── Download tests ──────────────────────────────────────────────────

/// Verifies that every `DownloadId` with a TOML definition has a fully populated entry.
///
/// Ensures all active freeware download entries carry a non-empty title and at least
/// one provided package, catching any `DownloadId` constant added without a matching
/// entry in the download table.
#[test]
fn all_active_download_ids_have_definitions() {
    let ids = [
        // RA
        DownloadId::RaQuickInstall,
        DownloadId::RaBaseFiles,
        DownloadId::RaAftermath,
        DownloadId::RaCncDesert,
        DownloadId::RaMusic,
        // RA Archive.org
        DownloadId::RaFullDiscs,
        DownloadId::RaFullSet,
        // TD
        DownloadId::TdBaseFiles,
        DownloadId::TdMusic,
        DownloadId::TdCovertOps,
        DownloadId::TdGdiIso,
        DownloadId::TdNodIso,
        // TS
        DownloadId::TsBaseFiles,
        DownloadId::TsQuickInstall,
        DownloadId::TsExpand,
        DownloadId::TsGdiIso,
        DownloadId::TsNodIso,
        DownloadId::TsFirestormIso,
        DownloadId::TsMusic,
    ];
    for id in ids {
        let dl = download(id).unwrap();
        assert_eq!(dl.id, id);
        assert!(!dl.title.is_empty());
        assert!(!dl.provides.is_empty());
    }
}

/// Verifies that future/planned `DownloadId` variants without mirrors have no TOML definition.
///
/// These IDs exist as enum variants for forward compatibility (future content ZIPs)
/// but have no entry in `downloads.toml` because no download path exists yet.
/// When mirrors go live, the ID moves from this list to `all_active_download_ids_have_definitions`.
#[test]
fn planned_download_ids_have_no_definition() {
    let planned = [
        DownloadId::RaMoviesAllied,
        DownloadId::RaMoviesSoviet,
        DownloadId::RaMusicCounterstrike,
        DownloadId::RaMusicAftermath,
        DownloadId::TdMoviesGdi,
        DownloadId::TdMoviesNod,
        DownloadId::TsMovies,
    ];
    for id in planned {
        assert!(
            download(id).is_none(),
            "Planned download {id:?} should not have a TOML definition yet",
        );
    }
}

/// Verifies the exact set of required Red Alert packages is Base, Aftermath, and CnC Desert.
///
/// The `required` flag drives install prompts and completeness checks; pinning the
/// expected list prevents silent additions or removals that would change what the
/// engine considers a minimum playable RA installation.
#[test]
fn ra_required_packages_are_base_aftermath_desert() {
    let required: Vec<PackageId> = packages::ALL_PACKAGES
        .iter()
        .filter(|p| p.game == GameId::RedAlert && p.required)
        .map(|p| p.id)
        .collect();
    assert_eq!(
        required,
        vec![
            PackageId::RaBase,
            PackageId::RaAftermathBase,
            PackageId::RaCncDesert
        ]
    );
}

/// Verifies that exactly one Tiberian Dawn package is marked required: the base package.
///
/// TD expansions (Covert Ops, music, movies) are optional; only the base data files
/// are needed to launch. This pins the invariant so future optional packages cannot
/// accidentally be flagged `required`.
#[test]
fn td_required_package_is_base() {
    let required: Vec<PackageId> = packages::ALL_PACKAGES
        .iter()
        .filter(|p| p.game == GameId::TiberianDawn && p.required)
        .map(|p| p.id)
        .collect();
    assert_eq!(required, vec![PackageId::TdBase]);
}

/// Verifies that every package belonging to a freeware game has an associated download ID.
///
/// Freeware packages must be automatically downloadable; a missing `download` field
/// would leave users with no installation path and no error message, silently breaking
/// the install flow.
#[test]
fn freeware_packages_have_downloads() {
    for pkg in packages::ALL_PACKAGES {
        if pkg.game.is_freeware() {
            assert!(
                pkg.download.is_some(),
                "Freeware package {:?} ({}) should have a download ID",
                pkg.id,
                pkg.title,
            );
        }
    }
}

/// Verifies that packages for non-freeware games carry no download ID.
///
/// Distributing non-freeware content via the automatic download path would be a legal
/// violation; this ensures the `download` field remains `None` for all commercial game
/// packages regardless of future table edits.
#[test]
fn non_freeware_packages_have_no_downloads() {
    for pkg in packages::ALL_PACKAGES {
        if !pkg.game.is_freeware() {
            assert!(
                pkg.download.is_none(),
                "Non-freeware package {:?} ({}) must not have a download ID",
                pkg.id,
                pkg.title,
            );
        }
    }
}

/// Verifies that all SHA-1 hashes in source identity-file entries are lowercase hex strings.
///
/// The verify path compares computed digests against stored values using simple string
/// equality; mixed-case or uppercase hex would cause false verification failures on
/// files that are actually correct.
#[test]
fn sha1_hashes_are_lowercase_hex() {
    for source in sources::ALL_SOURCES {
        for check in source.id_files {
            assert!(
                check
                    .sha1
                    .chars()
                    .all(|c| c.is_ascii_hexdigit() && !c.is_ascii_uppercase()),
                "SHA-1 for {} in source {:?} should be lowercase hex, got: {}",
                check.path,
                source.id,
                check.sha1,
            );
        }
    }
}

/// Verifies that every download's SHA-1 field, when present, is exactly 40 hexadecimal characters.
///
/// A SHA-1 digest is always 20 bytes / 40 hex chars; a shorter or non-hex value
/// indicates a data-entry error that would cause integrity checks to fail or panic
/// when parsed. Downloads without a SHA-1 (`None`) are not yet verified and are
/// skipped — they have no hash to validate.
#[test]
fn download_sha1_hashes_are_valid_hex() {
    for dl in downloads::all_downloads() {
        if let Some(sha1) = &dl.sha1 {
            assert_eq!(
                sha1.len(),
                40,
                "Download {:?} SHA-1 should be 40 hex chars, got {} chars",
                dl.id,
                sha1.len(),
            );
            assert!(
                sha1.chars().all(|c| c.is_ascii_hexdigit()),
                "Download {:?} SHA-1 should be hex, got: {}",
                dl.id,
                sha1,
            );
        }
    }
}

/// Verifies that every `SourceId` referenced in any package's `sources` list has a definition.
///
/// A dangling source reference would cause a panic at runtime when the engine tries
/// to detect or display that source; this catches the mismatch at compile-time test
/// granularity instead.
#[test]
fn every_package_source_exists() {
    for pkg in packages::ALL_PACKAGES {
        for &src_id in pkg.sources {
            assert!(
                source(src_id).is_some(),
                "Package {:?} references source {:?} which has no definition",
                pkg.id,
                src_id,
            );
        }
    }
}

/// Verifies that every `DownloadId` referenced by a package resolves and lists that package as provided.
///
/// Ensures bidirectional consistency: if a package says it can be obtained via a
/// given download, that download must reciprocally declare it provides that package.
/// Packages referencing a DownloadId without a TOML definition are skipped — the
/// definition will be added when mirrors go live.
#[test]
fn every_package_download_exists() {
    for pkg in packages::ALL_PACKAGES {
        if let Some(dl_id) = pkg.download {
            // Ghost DownloadIds (no TOML entry yet) are acceptable — skip them.
            if let Some(dl) = download(dl_id) {
                assert!(
                    dl.provides.contains(&pkg.id),
                    "Download {:?} should provide package {:?}",
                    dl_id,
                    pkg.id,
                );
            }
        }
    }
}
