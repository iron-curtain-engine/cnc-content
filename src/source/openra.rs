// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (c) 2025–present Iron Curtain contributors

//! OpenRA content probe — detects RA1 content already managed by OpenRA.
//!
//! OpenRA stores extracted content in `~/.openra/Content/ra/v2/` (or the
//! platform-equivalent). If OpenRA has already installed content, we can
//! reference those files instead of re-downloading.

use std::path::PathBuf;

use super::DetectedSource;
use crate::{PackageId, SourceId};

/// Source ID we use internally for OpenRA-sourced content.
/// This doesn't map to one of our 12 defined sources since OpenRA content
/// is already extracted — we just check if the files exist directly.
const OPENRA_EQUIVALENT_SOURCE: SourceId = SourceId::SteamTuc;

/// Probes for existing OpenRA content installations.
pub fn probe() -> Vec<DetectedSource> {
    let mut results = Vec::new();

    for content_path in openra_content_paths() {
        if !content_path.is_dir() {
            continue;
        }

        // Check which of our packages are satisfied by OpenRA's content dir.
        let mut provided = Vec::new();
        for pkg in crate::packages::ALL_PACKAGES {
            let all_present = pkg.test_files.iter().all(|f| content_path.join(f).exists());
            if all_present {
                provided.push(pkg.id);
            }
        }

        if !provided.is_empty() {
            results.push(DetectedSource {
                source_id: OPENRA_EQUIVALENT_SOURCE,
                path: content_path,
                packages: provided,
            });
        }
    }

    results
}

/// Returns platform-specific OpenRA content directory candidates.
fn openra_content_paths() -> Vec<PathBuf> {
    let mut paths = Vec::new();

    #[cfg(target_os = "windows")]
    {
        if let Ok(appdata) = std::env::var("APPDATA") {
            paths.push(
                PathBuf::from(appdata)
                    .join("OpenRA")
                    .join("Content")
                    .join("ra")
                    .join("v2"),
            );
        }
    }

    #[cfg(target_os = "linux")]
    {
        if let Ok(home) = std::env::var("HOME") {
            let home = PathBuf::from(home);
            paths.push(home.join(".openra/Content/ra/v2"));
            // Flatpak OpenRA.
            paths.push(home.join(".var/app/net.openra.OpenRA/.openra/Content/ra/v2"));
            // Snap OpenRA.
            paths.push(home.join("snap/openra/current/.openra/Content/ra/v2"));
        }
        if let Ok(xdg) = std::env::var("XDG_CONFIG_HOME") {
            paths.push(
                PathBuf::from(xdg)
                    .join("openra")
                    .join("Content")
                    .join("ra")
                    .join("v2"),
            );
        }
    }

    #[cfg(target_os = "macos")]
    {
        if let Ok(home) = std::env::var("HOME") {
            paths
                .push(PathBuf::from(home).join("Library/Application Support/OpenRA/Content/ra/v2"));
        }
    }

    paths
}

/// Checks which IC packages are already present in an OpenRA content dir.
#[allow(dead_code)]
pub fn check_openra_packages(openra_content: &std::path::Path) -> Vec<PackageId> {
    crate::packages::ALL_PACKAGES
        .iter()
        .filter(|pkg| {
            pkg.test_files
                .iter()
                .all(|f| openra_content.join(f).exists())
        })
        .map(|pkg| pkg.id)
        .collect()
}
