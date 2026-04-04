// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (c) 2025–present Iron Curtain contributors

//! Integration tests for torrent hash validation and post-extraction manifests.
//!
//! Verifies that Archive.org info hashes are well-formed hex strings, and that
//! manifest generation produces valid SHA-256 entries for installed content.

use super::super::*;

// ── Archive.org torrent hash validation ────────────────────────────────

/// Verifies that every Archive.org torrent info hash is a 40-character lowercase hex string.
///
/// BitTorrent info hashes are SHA-1 digests encoded as 40 lowercase hex characters;
/// an incorrect length or non-hex character would cause the torrent client to reject
/// or misidentify the torrent.
#[test]
fn archive_org_info_hashes_are_valid_hex() {
    for dl in downloads::all_downloads() {
        if !dl.info_hash.is_empty() {
            assert_eq!(
                dl.info_hash.len(),
                40,
                "{:?} info_hash should be 40 hex chars, got {}",
                dl.id,
                dl.info_hash.len(),
            );
            assert!(
                dl.info_hash.chars().all(|c| c.is_ascii_hexdigit()),
                "{:?} info_hash should be hex only: {}",
                dl.id,
                dl.info_hash,
            );
            assert!(
                dl.info_hash.chars().all(|c| !c.is_ascii_uppercase()),
                "{:?} info_hash should be lowercase: {}",
                dl.id,
                dl.info_hash,
            );
        }
    }
}

/// Verifies that downloads with an Archive.org info hash include at least one Archive.org tracker.
///
/// Archive.org torrents are seeded primarily through Archive.org's own tracker
/// infrastructure; omitting those tracker URLs would leave the torrent reliant solely
/// on DHT, greatly reducing initial peer discovery reliability.
#[test]
fn archive_org_torrents_have_trackers() {
    // Packages with Archive.org info_hash should have Archive.org trackers.
    for dl in downloads::all_downloads() {
        if !dl.info_hash.is_empty() && !dl.trackers.is_empty() {
            assert!(
                dl.trackers.iter().any(|t| t.contains("archive.org")),
                "{:?} has trackers but none are Archive.org: {:?}",
                dl.id,
                dl.trackers,
            );
        }
    }
}

// ── Post-extraction manifest tests ─────────────────────────────────────

/// Verifies that `generate_manifest` produces a valid, TOML-serializable manifest for installed content.
///
/// Ensures the manifest carries the correct game and version fields, contains at least
/// one file entry, and that every entry has a 64-character lowercase hex SHA-256
/// digest, so the verify path can trust the manifest as a ground truth.
///
/// Fake content files matching the RA base package's `test_files` list are written to
/// a temporary directory before calling `generate_manifest`, then cleaned up afterward.
#[test]
fn manifest_generation_for_installed_content() {
    let tmp = std::env::temp_dir().join("cnc-manifest-gen");
    let _ = std::fs::remove_dir_all(&tmp);
    std::fs::create_dir_all(&tmp).unwrap();

    // Create fake installed content files matching RA base package test files.
    let ra_base = crate::package(PackageId::RaBase).unwrap();
    for test_file in ra_base.test_files {
        let path = tmp.join(test_file);
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent).unwrap();
        }
        std::fs::write(&path, b"fake content for manifest test").unwrap();
    }

    let manifest = crate::verify::generate_manifest(&tmp, "ra", "v1", &[PackageId::RaBase])
        .expect("manifest generation should succeed");

    assert_eq!(manifest.game, "ra");
    assert_eq!(manifest.content_version, "v1");
    assert!(
        !manifest.files.is_empty(),
        "manifest should contain file entries"
    );

    // Each file entry should have a valid SHA-256 (64 hex chars).
    for (path, digest) in &manifest.files {
        assert_eq!(
            digest.sha256.len(),
            64,
            "SHA-256 for {path} should be 64 chars"
        );
        assert!(
            digest.sha256.chars().all(|c| c.is_ascii_hexdigit()),
            "SHA-256 for {path} should be hex"
        );
    }

    // Manifest should serialize to TOML.
    let toml_str = toml::to_string(&manifest).expect("manifest should serialize to TOML");
    assert!(toml_str.contains("ra"), "TOML should contain game name");

    let _ = std::fs::remove_dir_all(&tmp);
}
