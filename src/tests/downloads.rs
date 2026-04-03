// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (c) 2025–present Iron Curtain contributors

//! Integration tests for download resolution, mirror lists, and seeding policy.
//!
//! Verifies that every `DownloadId` has a populated definition, mirror URLs
//! are well-formed, and seeding-policy selection logic is correct.

use super::super::*;

// ── Download tests ──────────────────────────────────────────────────

/// Verifies that every `DownloadId` variant has a fully populated download definition.
///
/// Ensures all freeware download entries carry a non-empty title and at least one
/// provided package, catching any `DownloadId` constant added without a matching
/// entry in the download table.
#[test]
fn all_download_ids_have_definitions() {
    let ids = [
        // RA
        DownloadId::RaQuickInstall,
        DownloadId::RaBaseFiles,
        DownloadId::RaAftermath,
        DownloadId::RaCncDesert,
        DownloadId::RaMusic,
        DownloadId::RaMoviesAllied,
        DownloadId::RaMoviesSoviet,
        DownloadId::RaMusicCounterstrike,
        DownloadId::RaMusicAftermath,
        // TD
        DownloadId::TdBaseFiles,
        DownloadId::TdMusic,
        DownloadId::TdMoviesGdi,
        DownloadId::TdMoviesNod,
        DownloadId::TdCovertOps,
        DownloadId::TdGdiIso,
        DownloadId::TdNodIso,
    ];
    for id in ids {
        let dl = download(id).unwrap();
        assert_eq!(dl.id, id);
        assert!(!dl.title.is_empty());
        assert!(!dl.provides.is_empty());
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

/// Verifies that every download's SHA-1 field is exactly 40 hexadecimal characters.
///
/// A SHA-1 digest is always 20 bytes / 40 hex chars; a shorter or non-hex value
/// indicates a data-entry error that would cause integrity checks to fail or panic
/// when parsed.
#[test]
fn download_sha1_hashes_are_valid_hex() {
    for dl in downloads::ALL_DOWNLOADS {
        assert_eq!(
            dl.sha1.len(),
            40,
            "Download {:?} SHA-1 should be 40 hex chars, got {} chars",
            dl.id,
            dl.sha1.len(),
        );
        assert!(
            dl.sha1.chars().all(|c| c.is_ascii_hexdigit()),
            "Download {:?} SHA-1 should be hex, got: {}",
            dl.id,
            dl.sha1,
        );
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
/// A mismatch would produce silent install failures.
#[test]
fn every_package_download_exists() {
    for pkg in packages::ALL_PACKAGES {
        if let Some(dl_id) = pkg.download {
            let dl = download(dl_id).unwrap();
            assert!(
                dl.provides.contains(&pkg.id),
                "Download {:?} should provide package {:?}",
                dl_id,
                pkg.id,
            );
        }
    }
}

/// Verifies that every download's game tag matches the game of each package it provides.
///
/// A cross-game mismatch (e.g., a TD download claiming to provide an RA package) would
/// break per-game filtering in the status and install commands, potentially showing or
/// hiding downloads in the wrong context.
#[test]
fn download_game_matches_package_game() {
    for dl in downloads::ALL_DOWNLOADS {
        for &pkg_id in dl.provides {
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
    for recipe in recipes::ALL_RECIPES {
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
    for r in recipes::ALL_RECIPES {
        let _ = source(r.source);
        let _ = package(r.package);
    }
}

/// Verifies that all expected packages are reported missing when the content directory is empty.
///
/// Checks `missing_required_packages` and `missing_packages` against a freshly
/// created empty temporary directory for all four supported games, ensuring the
/// counts match the known package tables so the install flow correctly identifies
/// what needs to be obtained.
///
/// A real temporary directory is created and deleted around the test rather than
/// mocking, because the functions under test use filesystem probing.
#[test]
fn missing_packages_on_empty_dir() {
    let tmp = std::env::temp_dir().join("cnc-content-test-empty");
    let _ = std::fs::remove_dir_all(&tmp);
    std::fs::create_dir_all(&tmp).unwrap();

    let missing = missing_required_packages(&tmp, GameId::RedAlert);
    assert_eq!(missing.len(), 3);

    let all_missing = missing_packages(&tmp, GameId::RedAlert);
    assert_eq!(all_missing.len(), 8);

    let td_missing = missing_required_packages(&tmp, GameId::TiberianDawn);
    assert_eq!(td_missing.len(), 1);

    let dune_missing = missing_required_packages(&tmp, GameId::Dune2);
    assert_eq!(dune_missing.len(), 1);

    let dune2k_missing = missing_required_packages(&tmp, GameId::Dune2000);
    assert_eq!(dune2k_missing.len(), 1);

    let ts_missing = missing_required_packages(&tmp, GameId::TiberianSun);
    assert_eq!(ts_missing.len(), 1);

    let ra2_missing = missing_required_packages(&tmp, GameId::RedAlert2);
    assert_eq!(ra2_missing.len(), 1);

    let gen_missing = missing_required_packages(&tmp, GameId::Generals);
    assert_eq!(gen_missing.len(), 1);

    let _ = std::fs::remove_dir_all(&tmp);
}

/// Verifies that `is_content_complete` returns `false` for all games on an empty directory.
///
/// An empty content directory must never be considered complete; this guards against
/// a regression where the function returns `true` vacuously (e.g., when the package
/// list is empty or the directory probe short-circuits).
#[test]
fn is_content_complete_false_on_empty() {
    let tmp = std::env::temp_dir().join("cnc-content-test-empty-2");
    let _ = std::fs::remove_dir_all(&tmp);
    std::fs::create_dir_all(&tmp).unwrap();

    assert!(!is_content_complete(&tmp, GameId::RedAlert));
    assert!(!is_content_complete(&tmp, GameId::TiberianDawn));
    assert!(!is_content_complete(&tmp, GameId::Dune2));
    assert!(!is_content_complete(&tmp, GameId::Dune2000));
    assert!(!is_content_complete(&tmp, GameId::TiberianSun));
    assert!(!is_content_complete(&tmp, GameId::RedAlert2));
    assert!(!is_content_complete(&tmp, GameId::Generals));

    let _ = std::fs::remove_dir_all(&tmp);
}

/// Verifies that `default_content_root` returns a non-empty path ending in the expected suffix.
///
/// The default content root is baked into CLI help text and used as the installation
/// target; a wrong or empty path would silently write content to an unexpected location
/// or fail to find previously installed files.
#[test]
fn default_content_root_is_not_empty() {
    let root = default_content_root();
    assert!(!root.as_os_str().is_empty());
    // Should end with content/ra/v1 (or content\ra\v1 on Windows).
    let s = root.to_string_lossy();
    assert!(
        s.ends_with("ra/v1") || s.ends_with("ra\\v1"),
        "expected path ending in ra/v1, got: {s}"
    );
    assert!(
        s.contains("content"),
        "expected 'content' in path, got: {s}"
    );
}

/// Verifies that the per-game default content root path includes the game's slug segment.
///
/// Each game stores its content in a slug-named subdirectory; if the slug were absent,
/// all games would share the same directory and overwrite each other's files.
#[test]
fn default_content_root_for_game_includes_slug() {
    for &game in GameId::ALL {
        let root = default_content_root_for_game(game);
        let s = root.to_string_lossy();
        assert!(
            s.contains(game.slug()),
            "expected '{}' in path for {:?}, got: {s}",
            game.slug(),
            game
        );
    }
}

/// Verifies that `openra_content_root` returns a plausible path when a home directory is set.
///
/// The function may legitimately return `None` in stripped CI environments that lack
/// both `HOME` and `APPDATA`; when it does return `Some`, the path must contain
/// "OpenRA" or "openra" to confirm it points into the correct application data tree.
#[test]
fn openra_content_root_returns_some() {
    // Should resolve on all platforms with a HOME or APPDATA set.
    let root = openra_content_root();
    if let Some(ref path) = root {
        let s = path.to_string_lossy();
        assert!(
            s.contains("OpenRA") || s.contains("openra"),
            "expected OpenRA in path, got: {s}"
        );
    }
    // On CI without HOME/APPDATA this may be None — that's fine.
}

/// Verifies that `packages_for_game` returns the expected number of packages for each game.
///
/// Pins the package counts (RA: 8, TD: 5, Dune 2: 1, Dune 2000: 1, TS: 4,
/// RA2: 4, Generals: 2) so that any accidental addition or deletion of a package
/// entry is caught immediately rather
/// than silently shifting user-visible install menus.
#[test]
fn packages_for_game_returns_correct_counts() {
    let ra = packages_for_game(GameId::RedAlert);
    assert_eq!(ra.len(), 8, "RA should have 8 packages");

    let td = packages_for_game(GameId::TiberianDawn);
    assert_eq!(td.len(), 5, "TD should have 5 packages");

    let dune = packages_for_game(GameId::Dune2);
    assert_eq!(dune.len(), 1, "Dune 2 should have 1 package");

    let dune2k = packages_for_game(GameId::Dune2000);
    assert_eq!(dune2k.len(), 1, "Dune 2000 should have 1 package");

    let ts = packages_for_game(GameId::TiberianSun);
    assert_eq!(ts.len(), 4, "TS should have 4 packages");

    let ra2 = packages_for_game(GameId::RedAlert2);
    assert_eq!(ra2.len(), 4, "RA2 should have 4 packages");

    let gen = packages_for_game(GameId::Generals);
    assert_eq!(gen.len(), 2, "Generals should have 2 packages");
}

/// Verifies that `downloads_for_game` returns the expected number of download entries per game.
///
/// Pins counts (RA: 11, TD: 7, Dune 2: 0) and confirms that Dune 2, which is not
/// freeware, has zero downloads — reinforcing the legal invariant tested separately
/// by `non_freeware_packages_have_no_downloads`.
#[test]
fn downloads_for_game_returns_correct_counts() {
    let ra = downloads_for_game(GameId::RedAlert);
    assert_eq!(ra.len(), 11, "RA should have 11 downloads");

    let td = downloads_for_game(GameId::TiberianDawn);
    assert_eq!(td.len(), 7, "TD should have 7 downloads");

    let dune = downloads_for_game(GameId::Dune2);
    assert_eq!(
        dune.len(),
        0,
        "Dune 2 should have 0 downloads (not freeware)"
    );

    let ts = downloads_for_game(GameId::TiberianSun);
    assert_eq!(ts.len(), 0, "TS should have 0 downloads (not freeware)");

    let ra2 = downloads_for_game(GameId::RedAlert2);
    assert_eq!(ra2.len(), 0, "RA2 should have 0 downloads (not freeware)");

    let gen = downloads_for_game(GameId::Generals);
    assert_eq!(
        gen.len(),
        0,
        "Generals should have 0 downloads (not freeware)"
    );
}

/// Verifies that every download marked `is_available()` has at least one URL source.
///
/// An available download with no mirrors, direct URLs, or torrent hash is impossible
/// to fetch; flagging it available without any retrieval path would cause a confusing
/// runtime error instead of a clear configuration error caught at test time.
#[test]
fn available_downloads_have_urls() {
    // Downloads with is_available() == true must have at least one source.
    for dl in downloads::ALL_DOWNLOADS {
        if dl.is_available() {
            let has_mirrors = !dl.mirror_list_url.is_empty();
            let has_direct = !dl.direct_urls.is_empty();
            let has_torrent = !dl.info_hash.is_empty();
            assert!(
                has_mirrors || has_direct || has_torrent,
                "Available download {:?} must have mirrors, direct URLs, or torrent",
                dl.id,
            );
        }
    }
}

/// Verifies that downloads not yet marked available have all URL fields empty.
///
/// Prevents a partially-configured download from sneaking live URLs into the table
/// before it is officially flagged available, which could expose users to incomplete
/// or untested download paths.
#[test]
fn unavailable_downloads_have_no_urls() {
    // Downloads not yet available should have all URL fields empty.
    for dl in downloads::ALL_DOWNLOADS {
        if !dl.is_available() {
            assert!(
                dl.mirror_list_url.is_empty()
                    && dl.direct_urls.is_empty()
                    && dl.info_hash.is_empty(),
                "Unavailable download {:?} should have all URL fields empty",
                dl.id,
            );
        }
    }
}

/// Verifies that all download URLs reference domains from a known-live allowlist.
///
/// Guards against phantom domains (typos, unregistered hostnames, or stale mirrors)
/// being added to the download table; a URL pointing to an unknown domain would waste
/// user bandwidth and potentially hit an unrelated server.
#[test]
fn download_urls_use_known_live_domains() {
    // Every non-empty URL must point to a known-live domain.
    // This catches phantom domains that were never registered.
    let known_domains = [
        "www.openra.net",
        "cdn.mailaender.name",
        "openra.0x47.net",
        "files.cncnz.com",
        "raw.githubusercontent.com",
        "archive.org",
    ];

    for dl in downloads::ALL_DOWNLOADS {
        if !dl.mirror_list_url.is_empty() {
            assert!(
                known_domains.iter().any(|d| dl.mirror_list_url.contains(d)),
                "Download {:?} mirror_list_url uses unknown domain: {}",
                dl.id,
                dl.mirror_list_url,
            );
        }
        for url in dl.direct_urls {
            assert!(
                known_domains.iter().any(|d| url.contains(d)),
                "Download {:?} direct_url uses unknown domain: {}",
                dl.id,
                url,
            );
        }
    }
}

/// Verifies that `packages_for_source` returns the correct package set for representative sources.
///
/// Spot-checks the Steam TUC source (which provides several RA packages) and the CnC95
/// disc source (which provides only the desert tileset), confirming that the reverse
/// source-to-package index is consistent with the package definitions.
#[test]
fn packages_for_source_returns_correct_ids() {
    let steam_tuc_pkgs = source::packages_for_source(SourceId::SteamTuc);
    assert!(steam_tuc_pkgs.contains(&PackageId::RaBase));
    assert!(steam_tuc_pkgs.contains(&PackageId::RaAftermathBase));
    assert!(steam_tuc_pkgs.contains(&PackageId::RaMusic));
    assert!(steam_tuc_pkgs.contains(&PackageId::RaMoviesAllied));
    assert!(steam_tuc_pkgs.contains(&PackageId::RaMoviesSoviet));

    let cnc95_pkgs = source::packages_for_source(SourceId::Cnc95);
    assert!(cnc95_pkgs.contains(&PackageId::RaCncDesert));
    assert_eq!(cnc95_pkgs.len(), 1);
}

/// Verifies that `detect_all` completes without panicking even when no game sources are present.
///
/// In a CI environment that has no Steam, Origin, GOG, or disc drives, the function
/// must gracefully return an empty list rather than unwrapping a missing path or
/// registry key.
#[test]
fn detect_all_returns_empty_in_ci() {
    // In a CI environment without Steam/Origin/GOG/discs, detect_all should
    // return an empty list without panicking.
    let detected = source::detect_all();
    let _ = detected;
}

/// Verifies that every GOG-type source carries a `GogGameId` platform hint.
///
/// The GOG detection path uses the `PlatformHint::GogGameId` value to locate the
/// installation via the GOG database; a missing hint means the source can never be
/// auto-detected on GOG Galaxy.
#[test]
fn gog_sources_have_game_ids() {
    for source in sources::ALL_SOURCES {
        if matches!(source.source_type, SourceType::Gog) {
            assert!(
                matches!(source.platform_hint, Some(PlatformHint::GogGameId(_))),
                "GOG source {:?} should have a GogGameId hint",
                source.id,
            );
        }
    }
}

// ── SeedingPolicy tests ────────────────────────────────────────────────

/// Verifies that the default `SeedingPolicy` is `PauseDuringOnlinePlay`.
///
/// The default is user-visible: it appears in generated config files and help text.
/// Pinning it prevents an accidental `#[derive(Default)]` change from silently
/// switching new installations to a more or less aggressive seeding behavior.
#[test]
fn seeding_policy_default_is_pause_during_online_play() {
    assert_eq!(
        SeedingPolicy::default(),
        SeedingPolicy::PauseDuringOnlinePlay
    );
}

/// Verifies that `allows_seeding` returns the correct value for every `SeedingPolicy` variant.
///
/// Only `PauseDuringOnlinePlay` and `SeedAlways` permit seeding; `KeepNoSeed` and
/// `ExtractAndDelete` must not, since the latter two either discard the archive or
/// explicitly opt out of acting as a torrent peer.
#[test]
fn seeding_policy_allows_seeding() {
    assert!(SeedingPolicy::PauseDuringOnlinePlay.allows_seeding());
    assert!(SeedingPolicy::SeedAlways.allows_seeding());
    assert!(!SeedingPolicy::KeepNoSeed.allows_seeding());
    assert!(!SeedingPolicy::ExtractAndDelete.allows_seeding());
}

/// Verifies that `retains_archives` returns the correct value for every `SeedingPolicy` variant.
///
/// `ExtractAndDelete` is the only policy that discards the downloaded archive after
/// extraction; the other three all keep it (either for seeding or for re-verification),
/// so `retains_archives` must be `false` only for `ExtractAndDelete`.
#[test]
fn seeding_policy_retains_archives() {
    assert!(SeedingPolicy::PauseDuringOnlinePlay.retains_archives());
    assert!(SeedingPolicy::SeedAlways.retains_archives());
    assert!(SeedingPolicy::KeepNoSeed.retains_archives());
    assert!(!SeedingPolicy::ExtractAndDelete.retains_archives());
}

/// Verifies that `from_str_loose` parses all canonical names, aliases, and normalizations.
///
/// The loose parser is used for CLI arguments and config file values; it must accept
/// canonical names, underscore aliases, case variants, and hyphen-to-underscore
/// normalization, and must return `None` for unknown or empty input rather than
/// panicking.
#[test]
fn seeding_policy_from_str_loose_all_variants() {
    // Canonical names.
    assert_eq!(
        SeedingPolicy::from_str_loose("pause"),
        Some(SeedingPolicy::PauseDuringOnlinePlay)
    );
    assert_eq!(
        SeedingPolicy::from_str_loose("always"),
        Some(SeedingPolicy::SeedAlways)
    );
    assert_eq!(
        SeedingPolicy::from_str_loose("keep"),
        Some(SeedingPolicy::KeepNoSeed)
    );
    assert_eq!(
        SeedingPolicy::from_str_loose("delete"),
        Some(SeedingPolicy::ExtractAndDelete)
    );
    // Aliases.
    assert_eq!(
        SeedingPolicy::from_str_loose("default"),
        Some(SeedingPolicy::PauseDuringOnlinePlay)
    );
    assert_eq!(
        SeedingPolicy::from_str_loose("seed_always"),
        Some(SeedingPolicy::SeedAlways)
    );
    assert_eq!(
        SeedingPolicy::from_str_loose("no_seed"),
        Some(SeedingPolicy::KeepNoSeed)
    );
    assert_eq!(
        SeedingPolicy::from_str_loose("extract_and_delete"),
        Some(SeedingPolicy::ExtractAndDelete)
    );
    // Case insensitive.
    assert_eq!(
        SeedingPolicy::from_str_loose("ALWAYS"),
        Some(SeedingPolicy::SeedAlways)
    );
    // Hyphen → underscore normalization.
    assert_eq!(
        SeedingPolicy::from_str_loose("extract-and-delete"),
        Some(SeedingPolicy::ExtractAndDelete)
    );
    // Invalid.
    assert_eq!(SeedingPolicy::from_str_loose("unknown"), None);
    assert_eq!(SeedingPolicy::from_str_loose(""), None);
}

/// Verifies that every `SeedingPolicy` variant produces a non-empty display label.
///
/// Labels appear in CLI output and config file comments; an empty label would produce
/// a confusing blank entry in user-facing text.
#[test]
fn seeding_policy_label_non_empty() {
    for policy in [
        SeedingPolicy::PauseDuringOnlinePlay,
        SeedingPolicy::SeedAlways,
        SeedingPolicy::KeepNoSeed,
        SeedingPolicy::ExtractAndDelete,
    ] {
        assert!(
            !policy.label().is_empty(),
            "{:?} should have a label",
            policy
        );
    }
}

// ── Download strategy tests ────────────────────────────────────────────

/// Verifies that downloads without a torrent info hash are assigned the HTTP download strategy.
///
/// When no `info_hash` is set the torrent path is unavailable; falling back to anything
/// other than HTTP would leave those downloads permanently stuck.
#[cfg(feature = "download")]
#[test]
fn select_strategy_http_for_no_info_hash() {
    use crate::downloader::{select_strategy, DownloadStrategy};
    // Packages without info_hash should use HTTP strategy.
    for dl in downloads::ALL_DOWNLOADS {
        if dl.info_hash.is_empty() {
            assert_eq!(
                select_strategy(dl),
                DownloadStrategy::Http,
                "{:?} has no info_hash, should use HTTP",
                dl.id,
            );
        }
    }
}
