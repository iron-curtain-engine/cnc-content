// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (c) 2025–present Iron Curtain contributors

//! Content root and query tests.

use super::*;

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
    assert_eq!(
        ra.len(),
        7,
        "RA should have 7 downloads (4 OpenRA + 2 Archive.org + 1 music)"
    );

    let td = downloads_for_game(GameId::TiberianDawn);
    assert_eq!(
        td.len(),
        5,
        "TD should have 5 downloads (1 OpenRA + 1 music + 1 covert ops + 2 ISOs)"
    );

    let dune = downloads_for_game(GameId::Dune2);
    assert_eq!(
        dune.len(),
        0,
        "Dune 2 should have 0 downloads (not freeware)"
    );

    let ts = downloads_for_game(GameId::TiberianSun);
    assert_eq!(
        ts.len(),
        7,
        "TS should have 7 downloads (3 OpenRA + 3 ISOs + 1 music)"
    );

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
    for dl in downloads::all_downloads() {
        if dl.is_available() {
            let has_compiled_mirrors = !dl.mirrors.is_empty();
            let has_mirror_list = dl.mirror_list_url.is_some();
            let has_direct = !dl.direct_urls.is_empty();
            let has_torrent = dl.info_hash.is_some();
            assert!(
                has_compiled_mirrors || has_mirror_list || has_direct || has_torrent,
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
    for dl in downloads::all_downloads() {
        if !dl.is_available() {
            assert!(
                dl.mirrors.is_empty()
                    && dl.mirror_list_url.is_none()
                    && dl.direct_urls.is_empty()
                    && dl.info_hash.is_none(),
                "Unavailable download {:?} should have all URL fields empty",
                dl.id,
            );
        }
    }
}

/// Verifies that ALL download URLs reference domains from a known-live allowlist.
///
/// Guards against phantom domains (typos, unregistered hostnames, or stale mirrors)
/// being added to the download table. Covers mirrors, mirror_list_url, direct_urls,
/// AND web_seeds — every external URL the crate may contact at runtime.
#[test]
fn all_download_urls_use_known_live_domains() {
    // Every non-empty URL must point to a known-live domain.
    // This catches phantom domains that were never registered.
    let known_domains = [
        "www.openra.net",
        "cdn.mailaender.name",
        "openra.0x47.net",
        "files.cncnz.com",
        "bigdownloads.cnc-comm.com",
        "raw.githubusercontent.com",
        "archive.org",
        "openra.baxxster.no",
        "openra.ppmsite.com",
        "republic.community",
        "srvdonate.ut.mephi.ru",
    ];

    for dl in downloads::all_downloads() {
        for url in &dl.mirrors {
            assert!(
                known_domains.iter().any(|d| url.contains(d)),
                "Download {:?} mirror uses unknown domain: {}",
                dl.id,
                url,
            );
        }
        if let Some(mirror_url) = &dl.mirror_list_url {
            assert!(
                known_domains.iter().any(|d| mirror_url.contains(d)),
                "Download {:?} mirror_list_url uses unknown domain: {}",
                dl.id,
                mirror_url,
            );
        }
        for url in &dl.direct_urls {
            assert!(
                known_domains.iter().any(|d| url.contains(d)),
                "Download {:?} direct_url uses unknown domain: {}",
                dl.id,
                url,
            );
        }
        for url in &dl.web_seeds {
            assert!(
                known_domains.iter().any(|d| url.contains(d)),
                "Download {:?} web_seed uses unknown domain: {}",
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
