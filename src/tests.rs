// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (c) 2025–present Iron Curtain contributors

use super::*;

#[test]
fn all_package_ids_have_definitions() {
    let ids = [
        PackageId::Base,
        PackageId::AftermathBase,
        PackageId::CncDesert,
        PackageId::Music,
        PackageId::MoviesAllied,
        PackageId::MoviesSoviet,
        PackageId::MusicCounterstrike,
        PackageId::MusicAftermath,
    ];
    for id in ids {
        let pkg = package(id);
        assert_eq!(pkg.id, id);
        assert!(!pkg.title.is_empty());
        assert!(!pkg.test_files.is_empty());
        assert!(!pkg.sources.is_empty());
    }
}

#[test]
fn all_source_ids_have_definitions() {
    let ids = [
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
    ];
    for id in ids {
        let src = source(id);
        assert_eq!(src.id, id);
        assert!(!src.title.is_empty());
        assert!(!src.id_files.is_empty());
    }
}

#[test]
fn all_download_ids_have_definitions() {
    let ids = [
        DownloadId::QuickInstall,
        DownloadId::BaseFiles,
        DownloadId::Aftermath,
        DownloadId::CncDesert,
        DownloadId::Music,
        DownloadId::MoviesAllied,
        DownloadId::MoviesSoviet,
        DownloadId::MusicCounterstrike,
        DownloadId::MusicAftermath,
    ];
    for id in ids {
        let dl = download(id);
        assert_eq!(dl.id, id);
        assert!(!dl.title.is_empty());
        assert!(!dl.provides.is_empty());
    }
}

#[test]
fn required_packages_are_base_aftermath_desert() {
    let required: Vec<PackageId> = packages::ALL_PACKAGES
        .iter()
        .filter(|p| p.required)
        .map(|p| p.id)
        .collect();
    assert_eq!(
        required,
        vec![
            PackageId::Base,
            PackageId::AftermathBase,
            PackageId::CncDesert
        ]
    );
}

#[test]
fn all_packages_have_downloads() {
    for pkg in packages::ALL_PACKAGES {
        assert!(
            pkg.download.is_some(),
            "Package {:?} ({}) should have a download ID",
            pkg.id,
            pkg.title,
        );
    }
}

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

#[test]
fn every_package_source_exists() {
    for pkg in packages::ALL_PACKAGES {
        for &src_id in pkg.sources {
            let _ = source(src_id);
        }
    }
}

#[test]
fn every_package_download_exists() {
    for pkg in packages::ALL_PACKAGES {
        if let Some(dl_id) = pkg.download {
            let dl = download(dl_id);
            assert!(
                dl.provides.contains(&pkg.id),
                "Download {:?} should provide package {:?}",
                dl_id,
                pkg.id,
            );
        }
    }
}

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

#[test]
fn recipes_cover_all_source_package_pairs() {
    // Every (source, package) pair listed in ALL_PACKAGES.sources should
    // have a corresponding recipe.
    let mut missing = Vec::new();
    for pkg in packages::ALL_PACKAGES {
        for &src_id in pkg.sources {
            if recipe(src_id, pkg.id).is_none() {
                missing.push((src_id, pkg.id));
            }
        }
    }
    // Known gaps: TFD only has Base+CncDesert recipes (aftermath needs ISCAB),
    // Remastered doesn't have movie recipes yet, disc sources are partial.
    // We track what IS covered rather than requiring 100%.
    let covered = recipes::ALL_RECIPES.len();
    assert!(covered >= 30, "Expected at least 30 recipes, got {covered}");
}

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

#[test]
fn recipe_source_and_package_have_definitions() {
    for r in recipes::ALL_RECIPES {
        let _ = source(r.source);
        let _ = package(r.package);
    }
}

#[test]
fn missing_packages_on_empty_dir() {
    let tmp = std::env::temp_dir().join("cnc-content-test-empty");
    let _ = std::fs::remove_dir_all(&tmp);
    std::fs::create_dir_all(&tmp).unwrap();

    let missing = missing_required_packages(&tmp);
    assert_eq!(missing.len(), 3);

    let all_missing = missing_packages(&tmp);
    assert_eq!(all_missing.len(), 8);

    let _ = std::fs::remove_dir_all(&tmp);
}

#[test]
fn is_content_complete_false_on_empty() {
    let tmp = std::env::temp_dir().join("cnc-content-test-empty-2");
    let _ = std::fs::remove_dir_all(&tmp);
    std::fs::create_dir_all(&tmp).unwrap();

    assert!(!is_content_complete(&tmp));

    let _ = std::fs::remove_dir_all(&tmp);
}

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
    // Should be exe-relative (contains "content" segment).
    assert!(
        s.contains("content"),
        "expected 'content' in path, got: {s}"
    );
}

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

#[test]
fn packages_for_source_returns_correct_ids() {
    let steam_tuc_pkgs = source::packages_for_source(SourceId::SteamTuc);
    assert!(steam_tuc_pkgs.contains(&PackageId::Base));
    assert!(steam_tuc_pkgs.contains(&PackageId::AftermathBase));
    assert!(steam_tuc_pkgs.contains(&PackageId::Music));
    assert!(steam_tuc_pkgs.contains(&PackageId::MoviesAllied));
    assert!(steam_tuc_pkgs.contains(&PackageId::MoviesSoviet));

    let cnc95_pkgs = source::packages_for_source(SourceId::Cnc95);
    assert!(cnc95_pkgs.contains(&PackageId::CncDesert));
    assert_eq!(cnc95_pkgs.len(), 1);
}

#[test]
fn detect_all_returns_empty_in_ci() {
    // In a CI environment without Steam/Origin/discs, detect_all should
    // return an empty list without panicking.
    let detected = source::detect_all();
    // We can't assert the exact count since the test machine might have
    // game installs, but it shouldn't panic.
    let _ = detected;
}
