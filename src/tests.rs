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
    ];
    for id in ids {
        let dl = download(id);
        assert_eq!(dl.id, id);
        assert!(!dl.title.is_empty());
        assert!(!dl.sha1.is_empty());
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
fn missing_packages_on_empty_dir() {
    let tmp = std::env::temp_dir().join("cnc-content-test-empty");
    let _ = std::fs::remove_dir_all(&tmp);
    std::fs::create_dir_all(&tmp).unwrap();

    let missing = missing_required_packages(&tmp);
    assert_eq!(missing.len(), 3);

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
    // Should end with ra/v1 (or ra\v1 on Windows).
    let s = root.to_string_lossy();
    assert!(
        s.ends_with("ra/v1") || s.ends_with("ra\\v1"),
        "expected path ending in ra/v1, got: {s}"
    );
}
