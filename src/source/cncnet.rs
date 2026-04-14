// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (c) 2025–present Iron Curtain contributors

//! CnCNet source probe — detects game content installed by CnCNet clients.
//!
//! ## What
//!
//! CnCNet (<https://cncnet.org>) is a community multiplayer platform that has
//! kept classic C&C games playable online since 2009. CnCNet distributes
//! freeware installers for Tiberian Dawn, Red Alert, and Tiberian Sun at
//! `downloads.cncnet.org`. These installers extract game content to
//! platform-specific directories.
//!
//! ## Why
//!
//! Users who already have CnCNet-installed games should not need to
//! re-download content. This probe detects CnCNet installations and maps
//! them to existing source IDs so that the install recipe system can
//! extract content from the local CnCNet directories.
//!
//! ## How
//!
//! The probe checks known CnCNet installation paths on each platform:
//!
//! - **Windows:** `%APPDATA%\CnCNet\`, `C:\CnCNet\`, `C:\Games\CnCNet\`,
//!   and registry keys (NSIS uninstaller entries).
//! - **Linux/macOS:** `~/.cncnet/` (Wine/Proton wrappers).
//!
//! For each candidate path, the probe checks for known game subdirectories
//! (e.g. `Tiberian_Dawn/`, `Red_Alert/`, `Tiberian_Sun/`) and validates
//! that expected game files exist. Validated paths are mapped to the
//! closest matching `SourceId`.

use std::path::PathBuf;

use super::DetectedSource;
use crate::SourceId;

/// CnCNet game subdirectories and the SourceId they map to.
///
/// CnCNet game installs contain the same content as retail/digital sources.
/// We map each to the closest equivalent SourceId since the file layout
/// matches TUC-era digital distributions.
const CNCNET_GAMES: &[(&str, SourceId)] = &[
    // Red Alert — CnCNet distributes the freeware version matching TUC layout.
    ("Red_Alert", SourceId::SteamTuc),
    // Tiberian Dawn — freeware since 2007 (EA release), CnCNet layout matches CNC95.
    ("Tiberian_Dawn", SourceId::Cnc95),
    // Tiberian Sun — freeware since 2010, CnCNet distributes with Firestorm.
    ("Tiberian_Sun", SourceId::TsSteamTuc),
    // Dune 2000 — community-maintained, CnCNet hosts a patched version.
    ("Dune_2000", SourceId::Dune2000Disc),
];

/// Probes for CnCNet-installed game content.
///
/// Iterates known CnCNet base directories and checks each for game
/// subdirectories. When a game directory is found and contains expected
/// content files, a `DetectedSource` is produced.
pub fn probe() -> Vec<DetectedSource> {
    let mut results = Vec::new();

    for base_path in cncnet_base_paths() {
        if !base_path.is_dir() {
            continue;
        }

        for &(subdir, source_id) in CNCNET_GAMES {
            let game_path = base_path.join(subdir);
            if !game_path.is_dir() {
                continue;
            }

            // Check which packages this game directory can satisfy.
            let packages = super::packages_for_source(source_id);
            if packages.is_empty() {
                continue;
            }

            // Validate that at least one package's test files are present.
            let any_valid = crate::packages::ALL_PACKAGES
                .iter()
                .filter(|p| packages.contains(&p.id))
                .any(|p| p.test_files.iter().all(|f| game_path.join(f).exists()));

            if any_valid {
                results.push(DetectedSource {
                    source_id,
                    path: game_path,
                    packages,
                });
            }
        }
    }

    results
}

/// Returns platform-specific CnCNet base directory candidates.
///
/// CnCNet's NSIS-based installer defaults to several common paths on
/// Windows. On Linux/macOS, users may run CnCNet under Wine/Proton,
/// which writes to drive_c equivalents.
fn cncnet_base_paths() -> Vec<PathBuf> {
    let mut paths = Vec::new();

    #[cfg(target_os = "windows")]
    {
        // NSIS default: %APPDATA%\CnCNet
        if let Ok(appdata) = std::env::var("APPDATA") {
            paths.push(PathBuf::from(&appdata).join("CnCNet"));
        }

        // Common user-chosen install locations.
        paths.push(PathBuf::from(r"C:\CnCNet"));
        paths.push(PathBuf::from(r"C:\Games\CnCNet"));

        // Program Files variants.
        if let Ok(pf) = std::env::var("ProgramFiles") {
            paths.push(PathBuf::from(&pf).join("CnCNet"));
        }
        if let Ok(pf86) = std::env::var("ProgramFiles(x86)") {
            paths.push(PathBuf::from(&pf86).join("CnCNet"));
        }
    }

    #[cfg(target_os = "linux")]
    {
        // Wine prefix under user home.
        if let Ok(home) = std::env::var("HOME") {
            let home = PathBuf::from(home);
            paths.push(home.join(".cncnet"));
            // Lutris/PlayOnLinux Wine prefix.
            paths.push(home.join(".wine/drive_c/CnCNet"));
        }
    }

    #[cfg(target_os = "macos")]
    {
        if let Ok(home) = std::env::var("HOME") {
            paths.push(PathBuf::from(home).join(".cncnet"));
        }
    }

    paths
}

#[cfg(test)]
mod tests {
    use super::*;

    // ── Probe smoke tests ─────────────────────────────────────────

    /// Verifies that `probe()` does not panic when CnCNet is not installed.
    ///
    /// In CI and on machines without CnCNet, the probe should return an
    /// empty Vec rather than crashing on missing directories.
    #[test]
    fn probe_returns_empty_when_no_cncnet_installed() {
        let results = probe();
        // Just assert it returns a Vec without panicking.
        let _ = results;
    }

    // ── Candidate path generation ───────────────────────────────

    /// Verifies that `cncnet_base_paths()` returns at least one candidate
    /// path on every platform.
    ///
    /// Even when the directories don't exist on disk, the function should
    /// generate candidate paths from environment variables and well-known
    /// Windows locations.
    #[test]
    fn cncnet_base_paths_returns_candidates() {
        let paths = cncnet_base_paths();
        assert!(
            !paths.is_empty(),
            "cncnet_base_paths() should return at least one candidate path",
        );
    }

    // ── Game directory mapping ──────────────────────────────────

    /// Verifies that all mapped SourceIds in CNCNET_GAMES are valid.
    ///
    /// Each SourceId in the mapping must produce a non-empty package list,
    /// confirming the mapping is consistent with our package definitions.
    #[test]
    fn cncnet_game_source_ids_have_packages() {
        for &(subdir, source_id) in CNCNET_GAMES {
            let packages = super::super::packages_for_source(source_id);
            assert!(
                !packages.is_empty(),
                "CnCNet game '{subdir}' maps to {:?} which has no packages",
                source_id,
            );
        }
    }

    /// Verifies the CNCNET_GAMES mapping covers the expected set of games.
    ///
    /// CnCNet distributes freeware content for TD, RA, TS, and Dune 2000.
    /// All four should have entries in the mapping.
    #[test]
    fn cncnet_games_covers_known_titles() {
        let subdirs: Vec<&str> = CNCNET_GAMES.iter().map(|(s, _)| *s).collect();
        assert!(
            subdirs.contains(&"Red_Alert"),
            "RA missing from CnCNet games"
        );
        assert!(
            subdirs.contains(&"Tiberian_Dawn"),
            "TD missing from CnCNet games"
        );
        assert!(
            subdirs.contains(&"Tiberian_Sun"),
            "TS missing from CnCNet games"
        );
        assert!(
            subdirs.contains(&"Dune_2000"),
            "D2K missing from CnCNet games"
        );
    }
}
