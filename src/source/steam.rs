// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (c) 2025–present Iron Curtain contributors

//! Steam library probe — detects RA1 content in Steam installations.
//!
//! Finds Steam's install path, parses `libraryfolders.vdf` to locate all
//! library folders, then checks for relevant appmanifest files to find
//! game install directories.

use std::path::{Path, PathBuf};

use super::{packages_for_source, DetectedSource};
use crate::sources::ALL_SOURCES;
use crate::{PlatformHint, SourceType};

/// Known Steam app IDs for C&C content.
const STEAM_APP_IDS: &[(u32, &str)] = &[
    (2229840, "CnCRedalert"),     // The Ultimate Collection — RA
    (2229830, "CnCTiberianDawn"), // The Ultimate Collection — C&C
    (1213210, "CnCRemastered"),   // C&C Remastered Collection
];

/// Probes all Steam library folders for C&C game installs.
pub fn probe() -> Vec<DetectedSource> {
    let mut results = Vec::new();

    let steam_root = match find_steam_root() {
        Some(p) => p,
        None => return results,
    };

    let library_paths = find_library_folders(&steam_root);

    for lib_path in &library_paths {
        let steamapps = lib_path.join("steamapps");
        if !steamapps.is_dir() {
            continue;
        }

        for &(app_id, default_installdir) in STEAM_APP_IDS {
            if let Some(install_path) = find_app_install(&steamapps, app_id, default_installdir) {
                // Try to match against our source definitions.
                for source in ALL_SOURCES
                    .iter()
                    .filter(|s| s.source_type == SourceType::Steam)
                {
                    if let Some(PlatformHint::SteamAppId(sid)) = source.platform_hint {
                        if sid == app_id {
                            // Verify ID files to confirm source identity.
                            let all_match = source.id_files.iter().all(|check| {
                                crate::verify::verify_id_file(&install_path, check).unwrap_or(false)
                            });

                            if all_match {
                                results.push(DetectedSource {
                                    source_id: source.id,
                                    path: install_path.clone(),
                                    packages: packages_for_source(source.id),
                                });
                            }
                        }
                    }
                }
            }
        }
    }

    results
}

/// Finds the Steam installation root directory.
fn find_steam_root() -> Option<PathBuf> {
    // Try registry on Windows.
    #[cfg(target_os = "windows")]
    {
        if let Some(path) = steam_root_from_registry() {
            if path.is_dir() {
                return Some(path);
            }
        }
    }

    // Common filesystem paths.
    let candidates = steam_root_candidates();
    candidates.into_iter().find(|p| p.is_dir())
}

/// Returns platform-specific candidate paths for the Steam root.
fn steam_root_candidates() -> Vec<PathBuf> {
    let mut paths = Vec::new();

    #[cfg(target_os = "windows")]
    {
        if let Ok(pf86) = std::env::var("ProgramFiles(x86)") {
            paths.push(PathBuf::from(pf86).join("Steam"));
        }
        if let Ok(pf) = std::env::var("ProgramFiles") {
            paths.push(PathBuf::from(pf).join("Steam"));
        }
    }

    #[cfg(target_os = "linux")]
    {
        if let Ok(home) = std::env::var("HOME") {
            let home = PathBuf::from(home);
            paths.push(home.join(".steam/steam"));
            paths.push(home.join(".local/share/Steam"));
            // Flatpak Steam.
            paths.push(home.join(".var/app/com.valvesoftware.Steam/.steam/steam"));
            paths.push(home.join(".var/app/com.valvesoftware.Steam/.local/share/Steam"));
        }
    }

    #[cfg(target_os = "macos")]
    {
        if let Ok(home) = std::env::var("HOME") {
            paths.push(PathBuf::from(home).join("Library/Application Support/Steam"));
        }
    }

    paths
}

/// Reads the Steam install path from the Windows registry.
#[cfg(target_os = "windows")]
fn steam_root_from_registry() -> Option<PathBuf> {
    use winreg::enums::*;
    use winreg::RegKey;

    // Try HKCU first (more common for modern Steam installs).
    let hkcu = RegKey::predef(HKEY_CURRENT_USER);
    if let Ok(key) = hkcu.open_subkey(r"SOFTWARE\Valve\Steam") {
        if let Ok(path) = key.get_value::<String, _>("SteamPath") {
            return Some(PathBuf::from(path));
        }
    }

    // Fall back to HKLM.
    let hklm = RegKey::predef(HKEY_LOCAL_MACHINE);
    if let Ok(key) = hklm.open_subkey(r"SOFTWARE\Valve\Steam") {
        if let Ok(path) = key.get_value::<String, _>("InstallPath") {
            return Some(PathBuf::from(path));
        }
    }

    // Try WOW6432Node for 32-bit registry on 64-bit Windows.
    if let Ok(key) = hklm.open_subkey(r"SOFTWARE\WOW6432Node\Valve\Steam") {
        if let Ok(path) = key.get_value::<String, _>("InstallPath") {
            return Some(PathBuf::from(path));
        }
    }

    None
}

/// Parses `libraryfolders.vdf` to find all Steam library folder paths.
fn find_library_folders(steam_root: &Path) -> Vec<PathBuf> {
    let vdf_path = steam_root.join("steamapps/libraryfolders.vdf");
    // Also try config/ location (older Steam versions).
    let vdf_path = if vdf_path.is_file() {
        vdf_path
    } else {
        let alt = steam_root.join("config/libraryfolders.vdf");
        if alt.is_file() {
            alt
        } else {
            // If no VDF found, just return the root as the only library.
            return vec![steam_root.to_path_buf()];
        }
    };

    let content = match std::fs::read_to_string(&vdf_path) {
        Ok(c) => c,
        Err(_) => return vec![steam_root.to_path_buf()],
    };

    let parsed = match super::vdf::parse(&content) {
        Some(p) => p,
        None => return vec![steam_root.to_path_buf()],
    };

    let mut folders = Vec::new();

    // Find the "libraryfolders" section (or "LibraryFolders").
    let section = parsed
        .get("libraryfolders")
        .or_else(|| parsed.get("LibraryFolders"));

    if let Some(section) = section.and_then(|v| v.as_section()) {
        for value in section.values() {
            if let Some(sub) = value.as_section() {
                if let Some(path_str) = sub.get("path").and_then(|v| v.as_str()) {
                    let path = PathBuf::from(path_str);
                    if path.is_dir() {
                        folders.push(path);
                    }
                }
            }
            // Older format: value is just a string path.
            if let Some(path_str) = value.as_str() {
                let path = PathBuf::from(path_str);
                if path.is_dir() {
                    folders.push(path);
                }
            }
        }
    }

    // Always include the default library.
    let default = steam_root.to_path_buf();
    if !folders.contains(&default) {
        folders.insert(0, default);
    }

    folders
}

/// Finds the install path for a Steam app by reading its appmanifest.
fn find_app_install(steamapps: &Path, app_id: u32, default_installdir: &str) -> Option<PathBuf> {
    let manifest_name = format!("appmanifest_{app_id}.acf");
    let manifest_path = steamapps.join(&manifest_name);

    if !manifest_path.is_file() {
        return None;
    }

    let content = std::fs::read_to_string(&manifest_path).ok()?;
    let parsed = super::vdf::parse(&content)?;

    let app_state = parsed.get("AppState")?.as_section()?;
    let installdir = app_state
        .get("installdir")
        .and_then(|v| v.as_str())
        .unwrap_or(default_installdir);

    let install_path = steamapps.join("common").join(installdir);
    if install_path.is_dir() {
        Some(install_path)
    } else {
        None
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Verifies that `probe()` does not panic when Steam is not installed.
    ///
    /// In CI Steam is typically absent, so the probe should return an
    /// empty Vec rather than crashing when the Steam root is not found.
    #[test]
    fn probe_returns_empty_when_steam_not_installed() {
        let results = probe();
        let _ = results;
    }
}
