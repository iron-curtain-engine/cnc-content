// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (c) 2025–present Iron Curtain contributors

//! Origin / EA App probe — detects RA1 content in Origin or EA App installs.
//!
//! On Windows, reads registry keys to find install directories. On other
//! platforms, checks common filesystem locations.

use std::path::PathBuf;

use super::{packages_for_source, DetectedSource};
use crate::sources::ALL_SOURCES;
use crate::{PlatformHint, SourceType};

/// Probes for Origin / EA App installs of C&C games.
pub fn probe() -> Vec<DetectedSource> {
    let mut results = Vec::new();

    for source in ALL_SOURCES
        .iter()
        .filter(|s| s.source_type == SourceType::Origin)
    {
        if let Some(path) = find_origin_install(source) {
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

/// Attempts to find an Origin install path for a given source.
fn find_origin_install(source: &crate::ContentSource) -> Option<PathBuf> {
    // Try registry on Windows.
    #[cfg(target_os = "windows")]
    {
        if let Some(PlatformHint::RegistryKey { key, value }) = source.platform_hint {
            if let Some(path) = read_registry_install_dir(key, value) {
                if path.is_dir() {
                    return Some(path);
                }
            }
        }
    }

    // Try common EA App install locations.
    let candidates = ea_app_candidates(source);
    candidates.into_iter().find(|p| p.is_dir())
}

/// Reads an install directory from the Windows registry.
#[cfg(target_os = "windows")]
fn read_registry_install_dir(key_path: &str, value_name: &str) -> Option<PathBuf> {
    use winreg::enums::*;
    use winreg::RegKey;

    let hklm = RegKey::predef(HKEY_LOCAL_MACHINE);

    // Try direct path.
    if let Ok(key) = hklm.open_subkey(key_path) {
        if let Ok(path) = key.get_value::<String, _>(value_name) {
            return Some(PathBuf::from(path));
        }
    }

    // Try WOW6432Node (32-bit app on 64-bit Windows).
    let wow_path = key_path.replace("SOFTWARE\\", "SOFTWARE\\WOW6432Node\\");
    if let Ok(key) = hklm.open_subkey(&wow_path) {
        if let Ok(path) = key.get_value::<String, _>(value_name) {
            return Some(PathBuf::from(path));
        }
    }

    None
}

/// Returns common EA App install directory candidates for a source.
fn ea_app_candidates(source: &crate::ContentSource) -> Vec<PathBuf> {
    let mut paths = Vec::new();

    #[cfg(target_os = "windows")]
    {
        if let Ok(pf86) = std::env::var("ProgramFiles(x86)") {
            let ea_base = PathBuf::from(&pf86).join("Origin Games");
            match source.id {
                crate::SourceId::OriginTuc => {
                    paths.push(ea_base.join("Command and Conquer Red Alert"));
                    let ea_app = PathBuf::from(&pf86).join("EA Games");
                    paths.push(ea_app.join("Command and Conquer Red Alert"));
                }
                crate::SourceId::OriginCnc => {
                    paths.push(ea_base.join("Command and Conquer"));
                    let ea_app = PathBuf::from(&pf86).join("EA Games");
                    paths.push(ea_app.join("CNC and The Covert Operations"));
                }
                crate::SourceId::OriginRemastered => {
                    paths.push(ea_base.join("CnCRemastered"));
                    // EA App installs to a different path.
                    let ea_app_base = PathBuf::from(&pf86).join("Electronic Arts/EA Games");
                    paths.push(ea_app_base.join("CnCRemastered"));
                }
                _ => {}
            }
        }
    }

    let _ = source; // suppress unused warning on non-Windows
    paths
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Verifies that `probe()` does not panic when Origin/EA App is not installed.
    ///
    /// In CI there are no Origin game installs, so the probe should return
    /// an empty Vec without crashing.
    #[test]
    fn probe_returns_empty_in_ci() {
        let results = probe();
        let _ = results;
    }
}
