// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (c) 2025–present Iron Curtain contributors

//! Platform source detection — auto-discovers local game installs.
//!
//! Probes Steam libraries, Origin/EA App installs, OpenRA content directories,
//! and mounted disc volumes to find existing RA1 content that can be installed
//! from without downloading.

mod disc;
mod gog;
mod openra;
mod origin;
mod registry;
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
/// Runs Steam, Origin, GOG, registry, OpenRA, and disc probes in sequence.
/// Each probe returns zero or more detected sources. Results are deduplicated
/// by source ID (first detection wins).
pub fn detect_all() -> Vec<DetectedSource> {
    let mut results = Vec::new();
    let mut seen = std::collections::HashSet::new();

    for probe_fn in &[
        steam::probe as fn() -> Vec<DetectedSource>,
        origin::probe,
        gog::probe,
        registry::probe,
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

#[cfg(test)]
mod tests {
    use super::*;

    /// Verifies that `detect_all()` never returns duplicate source IDs.
    ///
    /// The deduplication logic in `detect_all` uses a `HashSet` to ensure
    /// first-detection-wins. This test confirms no source ID appears twice
    /// in the output, regardless of how many probes match.
    #[test]
    fn detect_all_deduplicates_by_source_id() {
        let results = detect_all();
        let mut seen = std::collections::HashSet::new();
        for detected in &results {
            assert!(
                seen.insert(detected.source_id),
                "duplicate source_id {:?} in detect_all() results",
                detected.source_id,
            );
        }
    }

    /// Verifies that every source ID referenced by at least one package
    /// produces a non-empty result from `packages_for_source()`.
    ///
    /// This catches stale source IDs in package definitions — if a source
    /// is listed in a package's `sources` but `packages_for_source` returns
    /// empty, the mapping is broken.
    #[test]
    fn packages_for_source_is_non_empty_for_known_sources() {
        let mut source_ids = std::collections::HashSet::new();
        for pkg in crate::packages::ALL_PACKAGES {
            for &sid in pkg.sources {
                source_ids.insert(sid);
            }
        }
        for sid in source_ids {
            let pkgs = packages_for_source(sid);
            assert!(
                !pkgs.is_empty(),
                "packages_for_source({:?}) returned empty, but it appears in at least one package",
                sid,
            );
        }
    }

    /// Verifies that `identify_at_path` returns `None` for an empty directory.
    ///
    /// An empty temp directory cannot match any source's ID files, so the
    /// function must return `None` rather than panicking or false-matching.
    #[test]
    fn identify_at_path_returns_none_for_empty_dir() {
        let tmp = std::env::temp_dir().join("cnc_content_test_empty_dir");
        std::fs::create_dir_all(&tmp).unwrap();
        assert!(identify_at_path(&tmp).is_none());
        let _ = std::fs::remove_dir(&tmp);
    }

    /// Verifies that `identify_at_path` returns `None` for a non-existent path.
    ///
    /// When given a path that does not exist on disk, the function must
    /// gracefully return `None` instead of panicking.
    #[test]
    fn identify_at_path_returns_none_for_nonexistent_path() {
        let path = std::path::Path::new("/tmp/cnc_content_test_nonexistent_path_42");
        assert!(identify_at_path(path).is_none());
    }
}
