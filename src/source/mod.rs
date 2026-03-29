// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (c) 2025–present Iron Curtain contributors

//! Platform source detection — auto-discovers local game installs.
//!
//! Probes Steam libraries, Origin/EA App installs, OpenRA content directories,
//! and mounted disc volumes to find existing RA1 content that can be installed
//! from without downloading.

mod disc;
mod openra;
mod origin;
mod steam;
mod vdf;

use std::path::PathBuf;

use crate::{PackageId, SourceId};

/// A detected content source on the local system.
#[derive(Debug, Clone)]
pub struct DetectedSource {
    /// Which source definition this matches.
    pub source_id: SourceId,
    /// Filesystem path to the source root.
    pub path: PathBuf,
    /// Packages this source can provide.
    pub packages: Vec<PackageId>,
}

/// Probes all known source locations and returns every detected source.
///
/// Runs Steam, Origin, OpenRA, and disc probes in sequence. Each probe
/// returns zero or more detected sources. Results are deduplicated by
/// source ID (first detection wins).
pub fn detect_all() -> Vec<DetectedSource> {
    let mut results = Vec::new();
    let mut seen = std::collections::HashSet::new();

    for probe_fn in &[
        steam::probe as fn() -> Vec<DetectedSource>,
        origin::probe,
        openra::probe,
        disc::probe,
    ] {
        for detected in probe_fn() {
            if seen.insert(detected.source_id) {
                results.push(detected);
            }
        }
    }

    results
}

/// Returns the packages a given source can provide, based on our package
/// definitions.
pub fn packages_for_source(source_id: SourceId) -> Vec<PackageId> {
    crate::packages::ALL_PACKAGES
        .iter()
        .filter(|p| p.sources.contains(&source_id))
        .map(|p| p.id)
        .collect()
}

/// Verifies a path against a source's ID files and returns the detected source
/// if all checks pass.
pub fn identify_at_path(path: &std::path::Path) -> Option<DetectedSource> {
    crate::verify::identify_source(path).map(|source_id| DetectedSource {
        source_id,
        path: path.to_path_buf(),
        packages: packages_for_source(source_id),
    })
}
