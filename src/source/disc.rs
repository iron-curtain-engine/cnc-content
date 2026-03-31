// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (c) 2025–present Iron Curtain contributors

//! Disc probe — detects RA1 content on mounted optical media or ISOs.
//!
//! Enumerates mounted volumes and checks for ID files that identify
//! specific C&C disc editions (Allied, Soviet, Aftermath, Counterstrike,
//! The First Decade).

use std::path::PathBuf;

use super::{packages_for_source, DetectedSource};
use crate::sources::ALL_SOURCES;
use crate::SourceType;

/// Probes mounted disc volumes for C&C content.
pub fn probe() -> Vec<DetectedSource> {
    let mut results = Vec::new();

    for mount_point in disc_mount_points() {
        if !mount_point.is_dir() {
            continue;
        }

        // Check each disc-type source against this mount point.
        for source in ALL_SOURCES
            .iter()
            .filter(|s| s.source_type == SourceType::Disc)
        {
            let all_match = source
                .id_files
                .iter()
                .all(|check| crate::verify::verify_id_file(&mount_point, check).unwrap_or(false));

            if all_match {
                results.push(DetectedSource {
                    source_id: source.id,
                    path: mount_point.clone(),
                    packages: packages_for_source(source.id),
                });
                break; // One source per mount point.
            }
        }
    }

    results
}

/// Returns candidate disc mount points for the current platform.
fn disc_mount_points() -> Vec<PathBuf> {
    let mut paths = Vec::new();

    #[cfg(target_os = "windows")]
    {
        // Check drive letters D: through Z: for optical drives.
        // On Windows, GetDriveType can identify CD-ROM drives, but for
        // simplicity we just check all letters since the ID file check
        // will filter non-matching volumes.
        for letter in b'D'..=b'Z' {
            let drive = format!("{}:\\", letter as char);
            let path = PathBuf::from(&drive);
            if path.is_dir() {
                paths.push(path);
            }
        }
    }

    #[cfg(target_os = "linux")]
    {
        // Standard mount points for optical media.
        paths.push(PathBuf::from("/mnt/cdrom"));
        paths.push(PathBuf::from("/media/cdrom"));
        paths.push(PathBuf::from("/media/cdrom0"));

        // User-mounted media under /media/$USER/.
        if let Ok(user) = std::env::var("USER") {
            let user_media = PathBuf::from(format!("/media/{user}"));
            if let Ok(entries) = std::fs::read_dir(&user_media) {
                for entry in entries.flatten() {
                    if entry.path().is_dir() {
                        paths.push(entry.path());
                    }
                }
            }
        }

        // /mnt/ subfolders.
        if let Ok(entries) = std::fs::read_dir("/mnt") {
            for entry in entries.flatten() {
                if entry.path().is_dir() {
                    paths.push(entry.path());
                }
            }
        }
    }

    #[cfg(target_os = "macos")]
    {
        // macOS mounts volumes under /Volumes/.
        if let Ok(entries) = std::fs::read_dir("/Volumes") {
            for entry in entries.flatten() {
                if entry.path().is_dir() {
                    paths.push(entry.path());
                }
            }
        }
    }

    paths
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Verifies that `probe()` does not panic when no disc volumes are mounted.
    ///
    /// In CI there are no C&C discs mounted, so the probe should return
    /// an empty Vec without crashing.
    #[test]
    fn probe_returns_empty_in_ci() {
        let results = probe();
        // Assert it returns a Vec without panicking.
        let _ = results;
    }
}
