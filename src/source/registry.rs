// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (c) 2025–present Iron Curtain contributors

//! Windows registry probe — detects C&C content via legacy Westwood/EA registry keys.
//!
//! Covers game installations that wrote their install paths to the Windows
//! registry but are NOT Origin-type (those are handled by `origin.rs`).
//! This includes the CnC95 freeware download, standalone RA installs from
//! the late 1990s, and The First Decade DVD installer.

#[cfg(target_os = "windows")]
use std::path::PathBuf;

use super::DetectedSource;

/// Probes Windows registry keys for legacy C&C game installs.
///
/// Iterates all source definitions that have `PlatformHint::RegistryKey`
/// and are NOT `SourceType::Origin` (those are handled by the Origin probe).
/// On non-Windows platforms, this returns an empty list.
#[cfg(not(target_os = "windows"))]
pub fn probe() -> Vec<DetectedSource> {
    Vec::new()
}

/// Probes Windows registry keys for legacy C&C game installs.
///
/// Iterates all source definitions that have `PlatformHint::RegistryKey`
/// and are NOT `SourceType::Origin` (those are handled by the Origin probe).
#[cfg(target_os = "windows")]
pub fn probe() -> Vec<DetectedSource> {
    use super::packages_for_source;
    use crate::sources::ALL_SOURCES;
    use crate::{PlatformHint, SourceType};

    let mut results = Vec::new();

    for source in ALL_SOURCES.iter().filter(|s| {
        s.source_type == SourceType::Registry
            && matches!(s.platform_hint, Some(PlatformHint::RegistryKey { .. }))
    }) {
        if let Some(path) = find_registry_install(source) {
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

/// Reads install directory from the Windows registry for a source.
#[cfg(target_os = "windows")]
fn find_registry_install(source: &crate::ContentSource) -> Option<PathBuf> {
    use crate::PlatformHint;

    if let Some(PlatformHint::RegistryKey { key, value }) = source.platform_hint {
        return read_registry_path(key, value);
    }
    None
}

/// Reads a path value from the Windows registry, trying both direct and
/// WOW6432Node paths.
#[cfg(target_os = "windows")]
fn read_registry_path(key_path: &str, value_name: &str) -> Option<PathBuf> {
    use winreg::enums::*;
    use winreg::RegKey;

    let hklm = RegKey::predef(HKEY_LOCAL_MACHINE);

    // Try direct path first.
    if let Ok(key) = hklm.open_subkey(key_path) {
        if let Ok(path) = key.get_value::<String, _>(value_name) {
            let p = PathBuf::from(&path);
            if p.is_dir() {
                return Some(p);
            }
        }
    }

    // Try HKCU (some installers write to current user).
    let hkcu = RegKey::predef(HKEY_CURRENT_USER);
    if let Ok(key) = hkcu.open_subkey(key_path) {
        if let Ok(path) = key.get_value::<String, _>(value_name) {
            let p = PathBuf::from(&path);
            if p.is_dir() {
                return Some(p);
            }
        }
    }

    // Try WOW6432Node (32-bit app on 64-bit Windows).
    let wow_path = key_path.replace("SOFTWARE\\", "SOFTWARE\\WOW6432Node\\");
    if let Ok(key) = hklm.open_subkey(&wow_path) {
        if let Ok(path) = key.get_value::<String, _>(value_name) {
            let p = PathBuf::from(&path);
            if p.is_dir() {
                return Some(p);
            }
        }
    }

    None
}

#[cfg(test)]
mod tests {
    use super::*;

    // ── Probe smoke tests ─────────────────────────────────────────

    /// Verifies that `probe()` does not panic when no legacy registry keys exist.
    ///
    /// In CI there are no Westwood/EA registry entries, so the probe should
    /// return an empty Vec without crashing. On non-Windows platforms, this
    /// always returns empty.
    #[test]
    fn probe_returns_empty_in_ci() {
        let results = probe();
        let _ = results;
    }
}
