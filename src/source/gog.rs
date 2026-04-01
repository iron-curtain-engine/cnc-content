// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (c) 2025–present Iron Curtain contributors

//! GOG.com probe — detects C&C content in GOG Galaxy and classic GOG installs.
//!
//! On Windows, checks the registry under `HKLM\SOFTWARE\GOG.com\Games\<id>`
//! and GOG Galaxy's default install paths. On Linux/macOS, checks common
//! filesystem locations where GOG installers place games.

use std::path::PathBuf;

use super::{packages_for_source, DetectedSource};
use crate::sources::ALL_SOURCES;
#[cfg(target_os = "windows")]
use crate::PlatformHint;
use crate::SourceType;

/// Probes for GOG.com installs of C&C games.
pub fn probe() -> Vec<DetectedSource> {
    let mut results = Vec::new();

    for source in ALL_SOURCES
        .iter()
        .filter(|s| s.source_type == SourceType::Gog)
    {
        if let Some(path) = find_gog_install(source) {
            let all_match = source
                .id_files
                .iter()
                .all(|check| crate::verify::verify_id_file(&path, check).unwrap_or(false));

            if all_match {
                results.push(DetectedSource {
                    source_id: source.id,
                    path,
                    packages: packages_for_source(source.id),
                });
            }
        }
    }

    results
}

/// Attempts to find a GOG install path for a given source.
fn find_gog_install(source: &crate::ContentSource) -> Option<PathBuf> {
    // Try GOG registry on Windows.
    #[cfg(target_os = "windows")]
    {
        if let Some(PlatformHint::GogGameId(game_id)) = source.platform_hint {
            if let Some(path) = gog_path_from_registry(game_id) {
                if path.is_dir() {
                    return Some(path);
                }
            }
        }
    }

    // Try common GOG filesystem locations.
    let candidates = gog_filesystem_candidates(source);
    candidates.into_iter().find(|p| p.is_dir())
}

/// Reads a GOG game's install path from the Windows registry.
///
/// GOG classic stores install paths at:
/// `HKLM\SOFTWARE\GOG.com\Games\<game_id>` → `path` value.
#[cfg(target_os = "windows")]
fn gog_path_from_registry(game_id: u64) -> Option<PathBuf> {
    use winreg::enums::*;
    use winreg::RegKey;

    let hklm = RegKey::predef(HKEY_LOCAL_MACHINE);

    let key_path = format!(r"SOFTWARE\GOG.com\Games\{game_id}");
    if let Ok(key) = hklm.open_subkey(&key_path) {
        if let Ok(path) = key.get_value::<String, _>("path") {
            return Some(PathBuf::from(path));
        }
    }

    // Try WOW6432Node (32-bit app on 64-bit Windows).
    let wow_path = format!(r"SOFTWARE\WOW6432Node\GOG.com\Games\{game_id}");
    if let Ok(key) = hklm.open_subkey(&wow_path) {
        if let Ok(path) = key.get_value::<String, _>("path") {
            return Some(PathBuf::from(path));
        }
    }

    None
}

/// Returns common GOG install directory candidates for a source.
fn gog_filesystem_candidates(source: &crate::ContentSource) -> Vec<PathBuf> {
    let mut paths = Vec::new();

    // Determine the expected game folder name from the source.
    let folder_name = match source.id {
        crate::SourceId::GogDune2 => "Dune 2 - The Building of a Dynasty",
        crate::SourceId::GogDune2000 => "Dune 2000",
        _ => return paths,
    };

    #[cfg(target_os = "windows")]
    {
        // GOG Galaxy default library.
        if let Ok(pf86) = std::env::var("ProgramFiles(x86)") {
            paths.push(
                PathBuf::from(&pf86)
                    .join("GOG Galaxy/Games")
                    .join(folder_name),
            );
        }
        if let Ok(pf) = std::env::var("ProgramFiles") {
            paths.push(PathBuf::from(pf).join("GOG Galaxy/Games").join(folder_name));
        }
        // GOG classic default location.
        paths.push(PathBuf::from(r"C:\GOG Games").join(folder_name));
    }

    #[cfg(target_os = "linux")]
    {
        if let Ok(home) = std::env::var("HOME") {
            let home = PathBuf::from(home);
            paths.push(home.join("GOG Games").join(folder_name));
            // Wine prefix used by GOG Linux installers.
            paths.push(home.join(".wine/drive_c/GOG Games").join(folder_name));
        }
    }

    #[cfg(target_os = "macos")]
    {
        if let Ok(home) = std::env::var("HOME") {
            paths.push(
                PathBuf::from(home)
                    .join("Library/Application Support/GOG.com")
                    .join(folder_name),
            );
        }
    }

    paths
}

#[cfg(test)]
mod tests {
    use super::*;

    // ── Probe smoke tests ─────────────────────────────────────────

    /// Verifies that `probe()` does not panic when no GOG installs are found.
    ///
    /// In CI there are no GOG games installed, so the probe should return
    /// an empty Vec without crashing.
    #[test]
    fn probe_returns_empty_in_ci() {
        let results = probe();
        let _ = results;
    }

    // ── Candidate path generation ───────────────────────────────

    /// Verifies that `gog_filesystem_candidates()` returns non-empty candidate
    /// paths for each GOG source definition.
    ///
    /// On Windows, GOG candidates include `C:\GOG Games` and GOG Galaxy paths.
    /// On Linux/macOS, candidates include `~/GOG Games` and Wine prefixes.
    /// This ensures the candidate generation logic is wired up correctly for
    /// each GOG source.
    #[test]
    fn gog_filesystem_candidates_returns_paths() {
        for source in crate::sources::ALL_SOURCES
            .iter()
            .filter(|s| s.source_type == SourceType::Gog)
        {
            let candidates = gog_filesystem_candidates(source);
            assert!(
                !candidates.is_empty(),
                "gog_filesystem_candidates for {:?} should return at least one candidate path",
                source.id,
            );
        }
    }
}
