// SPDX-License-Identifier: MIT OR Apache-2.0

//! Index backends for Workshop package discovery.
//!
//! The Workshop index maps `publisher/name` identities to their published
//! version manifests. This module defines the `IndexBackend` trait that
//! abstracts over different index sources, enabling the phased delivery
//! strategy from D030:
//!
//! - **Phase 0–3:** `GitIndexBackend` — a shallow-cloned git repo with
//!   one file per package (crates.io-index style). Updates via
//!   `git fetch --depth=1`. Zero infrastructure cost.
//!
//! - **Phase 4–5:** `HttpIndexBackend` — a REST API backed by a Workshop
//!   server. Richer queries, real-time updates, moderation.
//!
//! - **Phase 6a:** `FederatedIndexBackend` — multiple indexes merged via
//!   trust delegation. Community-run registries.
//!
//! All backends return the same `PackageManifest` type, so the transport
//! and integrity layers don't care where the metadata came from.
//!
//! # Git-index layout (Phase 0–3)
//!
//! ```text
//! workshop-index/
//!   config.json       ← index metadata (version, API URL)
//!   1/                ← 1-char package names
//!     a               ← file for package "a"
//!   2/                ← 2-char names
//!     ab
//!   3/                ← 3-char names (first char subdir)
//!     a/
//!       abc
//!   co/               ← 4+ char names (first-two/next-two sharding)
//!     mm/
//!       community-sprites  ← one line per version (JSON)
//! ```
//!
//! This layout is borrowed from crates.io. It distributes files across
//! directories to avoid filesystem bottlenecks, and each file contains
//! one JSON object per line (one per published version), making `git diff`
//! and append-only updates trivial.
//!
//! # Platform independence
//!
//! The git-index layout works with **any** git hosting platform:
//! GitHub, GitLab, Codeberg, Gitea, self-hosted bare repos, or even
//! a USB stick. The [`FailoverIndex`] combinator wraps multiple
//! `IndexBackend` implementations and tries them in priority order.
//! If the primary host becomes unavailable, clients automatically
//! fall back to the next mirror. Platform migration requires only
//! updating the backend list — no code changes, no recompilation.

use crate::error::WorkshopError;
use crate::manifest::PackageManifest;
use crate::resource::ResourceId;

// ── Update result ────────────────────────────────────────────────────

/// Summary of an index sync operation.
///
/// Returned by `IndexBackend::update()` to report what changed since the
/// last sync. This allows the caller to show progress or trigger
/// re-downloads for updated packages.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct UpdateResult {
    /// Number of new packages discovered (not previously known).
    pub new_packages: u64,
    /// Number of packages with new versions added.
    pub updated_packages: u64,
    /// Number of packages removed (revoked, DMCA, malware).
    pub removed_packages: u64,
}

impl UpdateResult {
    /// Returns true if nothing changed.
    pub fn is_empty(&self) -> bool {
        self.new_packages == 0 && self.updated_packages == 0 && self.removed_packages == 0
    }

    /// Total number of changes.
    pub fn total_changes(&self) -> u64 {
        self.new_packages
            .saturating_add(self.updated_packages)
            .saturating_add(self.removed_packages)
    }
}

// ── Index backend trait ──────────────────────────────────────────────

/// Abstraction over Workshop index sources.
///
/// All backends provide the same read interface: list packages, get
/// manifests for a package, and sync with the upstream source. The
/// game integration layer calls these methods without knowing whether
/// the index is a git repo, REST API, or federated merge.
///
/// Write operations (publish, revoke) are not part of this trait —
/// they go through CI pipelines or the Workshop server API, never
/// through the runtime client.
pub trait IndexBackend {
    /// Lists all known resource identifiers in the index.
    ///
    /// Returns the full set of `publisher/name` pairs. For large indexes,
    /// consider adding pagination in a future revision.
    fn list(&self) -> Result<Vec<ResourceId>, WorkshopError>;

    /// Returns all published version manifests for a resource.
    ///
    /// Versions are returned in chronological order (oldest first).
    /// Returns `Ok(None)` if the resource is not in the index.
    fn get(&self, id: &ResourceId) -> Result<Option<Vec<PackageManifest>>, WorkshopError>;

    /// Syncs with the upstream source and returns what changed.
    ///
    /// For git-index: `git fetch --depth=1` + diff.
    /// For HTTP: poll the API for changes since last sync.
    fn update(&mut self) -> Result<UpdateResult, WorkshopError>;
}

// ── In-memory index (for tests) ──────────────────────────────────────

/// In-memory index backend for unit testing. Not for production use.
#[derive(Debug, Default)]
pub struct MemoryIndex {
    packages: std::collections::HashMap<ResourceId, Vec<PackageManifest>>,
}

impl MemoryIndex {
    /// Inserts a manifest into the index.
    pub fn insert(&mut self, manifest: PackageManifest) {
        self.packages
            .entry(manifest.id().clone())
            .or_default()
            .push(manifest);
    }
}

impl IndexBackend for MemoryIndex {
    fn list(&self) -> Result<Vec<ResourceId>, WorkshopError> {
        Ok(self.packages.keys().cloned().collect())
    }

    fn get(&self, id: &ResourceId) -> Result<Option<Vec<PackageManifest>>, WorkshopError> {
        Ok(self.packages.get(id).cloned())
    }

    fn update(&mut self) -> Result<UpdateResult, WorkshopError> {
        // In-memory index is already up to date — no-op.
        Ok(UpdateResult {
            new_packages: 0,
            updated_packages: 0,
            removed_packages: 0,
        })
    }
}
// ── Failover index ───────────────────────────────────────────────────────

/// Multiple index backends tried in priority order for platform resilience.
///
/// All backends are assumed to be mirrors of the same index content.
/// Queries (`list`, `get`) try each backend in order and return the
/// first successful result. If all backends fail, returns
/// [`WorkshopError::AllSourcesFailed`]. Updates follow the same
/// failover strategy.
///
/// # Platform independence
///
/// This is the key mechanism for avoiding single-platform lock-in.
/// A typical production configuration:
///
/// ```text
/// FailoverIndex [
///   GitIndex("https://github.com/org/workshop-index.git"),     // primary
///   GitIndex("https://codeberg.org/org/workshop-index.git"),   // mirror 1
///   GitIndex("https://gitea.selfhost.org/org/workshop-index"), // mirror 2
///   HttpIndex("https://api.workshop.example.com/v1"),          // API fallback
/// ]
/// ```
///
/// If any host disappears, clients fail over to the next available
/// mirror automatically. Migration between platforms requires only
/// updating the backend list — no code changes, no recompilation.
pub struct FailoverIndex {
    backends: Vec<Box<dyn IndexBackend>>,
}

impl FailoverIndex {
    /// Creates a failover index from multiple backends, tried in order.
    ///
    /// The first backend in the list has highest priority.
    /// Returns an error if the backend list is empty.
    pub fn new(backends: Vec<Box<dyn IndexBackend>>) -> Result<Self, WorkshopError> {
        if backends.is_empty() {
            return Err(WorkshopError::Index {
                detail: "FailoverIndex requires at least one backend".to_string(),
            });
        }
        Ok(Self { backends })
    }

    /// Returns the number of configured backends.
    pub fn backend_count(&self) -> usize {
        self.backends.len()
    }
}

impl IndexBackend for FailoverIndex {
    fn list(&self) -> Result<Vec<ResourceId>, WorkshopError> {
        let mut last_error = None;
        for backend in &self.backends {
            match backend.list() {
                Ok(ids) => return Ok(ids),
                Err(e) => last_error = Some(e),
            }
        }
        Err(WorkshopError::AllSourcesFailed {
            count: self.backends.len(),
            last_error: last_error.map(|e| e.to_string()).unwrap_or_default(),
        })
    }

    fn get(&self, id: &ResourceId) -> Result<Option<Vec<PackageManifest>>, WorkshopError> {
        let mut last_error = None;
        for backend in &self.backends {
            match backend.get(id) {
                Ok(result) => return Ok(result),
                Err(e) => last_error = Some(e),
            }
        }
        Err(WorkshopError::AllSourcesFailed {
            count: self.backends.len(),
            last_error: last_error.map(|e| e.to_string()).unwrap_or_default(),
        })
    }

    fn update(&mut self) -> Result<UpdateResult, WorkshopError> {
        let mut last_error = None;
        for backend in &mut self.backends {
            match backend.update() {
                Ok(result) => return Ok(result),
                Err(e) => last_error = Some(e),
            }
        }
        Err(WorkshopError::AllSourcesFailed {
            count: self.backends.len(),
            last_error: last_error.map(|e| e.to_string()).unwrap_or_default(),
        })
    }
}
// ── Tests ────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;
    use crate::blob::BlobId;
    use crate::resource::ResourceVersion;

    /// Helper: creates a test manifest.
    fn test_manifest(publisher: &str, name: &str, major: u32) -> PackageManifest {
        PackageManifest::new(
            ResourceId::new(publisher, name).unwrap(),
            ResourceVersion::new(major, 0, 0),
            BlobId::new([major as u8; 32]),
            1024,
        )
    }

    // ── UpdateResult ─────────────────────────────────────────────────

    /// Empty update result reports correctly.
    #[test]
    fn update_result_empty() {
        let result = UpdateResult {
            new_packages: 0,
            updated_packages: 0,
            removed_packages: 0,
        };
        assert!(result.is_empty());
        assert_eq!(result.total_changes(), 0);
    }

    /// Non-empty update result counts changes.
    #[test]
    fn update_result_with_changes() {
        let result = UpdateResult {
            new_packages: 5,
            updated_packages: 3,
            removed_packages: 1,
        };
        assert!(!result.is_empty());
        assert_eq!(result.total_changes(), 9);
    }

    // ── MemoryIndex ──────────────────────────────────────────────────

    /// List returns all known resource IDs.
    #[test]
    fn memory_index_list() {
        let mut index = MemoryIndex::default();
        index.insert(test_manifest("alice", "sprites", 1));
        index.insert(test_manifest("bob", "maps", 1));

        let ids = index.list().unwrap();
        assert_eq!(ids.len(), 2);
    }

    /// Get returns all versions for a resource in insertion order.
    #[test]
    fn memory_index_get_versions() {
        let mut index = MemoryIndex::default();
        index.insert(test_manifest("alice", "sprites", 1));
        index.insert(test_manifest("alice", "sprites", 2));

        let id = ResourceId::new("alice", "sprites").unwrap();
        let versions = index.get(&id).unwrap().unwrap();
        assert_eq!(versions.len(), 2);
        assert_eq!(versions[0].version().major(), 1);
        assert_eq!(versions[1].version().major(), 2);
    }

    /// Get returns None for unknown resources.
    #[test]
    fn memory_index_get_missing() {
        let index = MemoryIndex::default();
        let id = ResourceId::new("nobody", "nothing").unwrap();
        assert!(index.get(&id).unwrap().is_none());
    }

    /// Update on memory index is a no-op.
    #[test]
    fn memory_index_update_noop() {
        let mut index = MemoryIndex::default();
        let result = index.update().unwrap();
        assert!(result.is_empty());
    }

    // ── Integration: index provides P2P download metadata ────────────

    /// Demonstrates the full flow: index → manifest → P2P download params.
    ///
    /// This is the key integration point between the three layers:
    /// 1. Index provides the manifest (this crate)
    /// 2. Manifest contains info_hash + web_seeds (bridge data)
    /// 3. p2p-distribute uses those to start the download (transport)
    #[test]
    fn index_to_p2p_flow() {
        let mut index = MemoryIndex::default();

        // Publisher uploads a mod with P2P metadata.
        // Mod served from multiple platforms — no single point of failure.
        let manifest = test_manifest("community", "hd-tanks", 1)
            .with_info_hash([0x42; 20])
            .with_web_seeds(vec![
                "https://mirror-a.example.com/hd-tanks-1.0.0.icpkg".to_string()
            ])
            .with_download_urls(vec![
                "https://mirror-a.example.com/hd-tanks-1.0.0.icpkg".to_string(),
                "https://mirror-b.example.org/hd-tanks-1.0.0.icpkg".to_string(),
            ]);
        index.insert(manifest);

        // Game client queries the index.
        let id = ResourceId::new("community", "hd-tanks").unwrap();
        let versions = index.get(&id).unwrap().unwrap();
        let latest = versions.last().unwrap();

        // Extract the data needed to start a P2P download:
        let _info_hash = latest.info_hash().expect("has torrent metadata");
        let _web_seeds = latest.web_seeds();
        let _fallback_urls = latest.download_urls();

        // These would be passed to p2p-distribute's download API.
        // The game integration layer wires this up.
        assert!(latest.info_hash().is_some());
        assert!(!latest.web_seeds().is_empty());
        assert!(latest.download_urls().len() >= 2, "multi-platform URLs");
    }

    // ── FailoverIndex ────────────────────────────────────────────────────

    /// Always-failing index backend for testing failover behavior.
    struct FailingIndex;

    impl IndexBackend for FailingIndex {
        fn list(&self) -> Result<Vec<ResourceId>, WorkshopError> {
            Err(WorkshopError::Index {
                detail: "simulated failure".to_string(),
            })
        }
        fn get(&self, _id: &ResourceId) -> Result<Option<Vec<PackageManifest>>, WorkshopError> {
            Err(WorkshopError::Index {
                detail: "simulated failure".to_string(),
            })
        }
        fn update(&mut self) -> Result<UpdateResult, WorkshopError> {
            Err(WorkshopError::Index {
                detail: "simulated failure".to_string(),
            })
        }
    }

    /// FailoverIndex requires at least one backend.
    #[test]
    fn failover_rejects_empty_backends() {
        let result = FailoverIndex::new(vec![]);
        assert!(result.is_err());
    }

    /// FailoverIndex skips failed backends and uses the first working one.
    ///
    /// This is the core platform resilience mechanism: if the primary
    /// mirror goes down, the client automatically uses the next available
    /// mirror. No code changes, no recompilation.
    #[test]
    fn failover_skips_failed_backend() {
        let mut index = FailoverIndex::new(vec![
            Box::new(FailingIndex),
            Box::new({
                let mut m = MemoryIndex::default();
                m.insert(test_manifest("alice", "sprites", 1));
                m
            }),
        ])
        .unwrap();

        // list() skips the failing backend and returns from the second.
        let ids = index.list().unwrap();
        assert_eq!(ids.len(), 1);

        // get() also fails over.
        let id = ResourceId::new("alice", "sprites").unwrap();
        let versions = index.get(&id).unwrap().unwrap();
        assert_eq!(versions.len(), 1);

        // update() also fails over.
        let result = index.update().unwrap();
        assert!(result.is_empty());
    }

    /// When all backends fail, FailoverIndex reports the aggregate error.
    #[test]
    fn failover_all_fail_reports_error() {
        let index =
            FailoverIndex::new(vec![Box::new(FailingIndex), Box::new(FailingIndex)]).unwrap();

        let err = index.list().unwrap_err();
        assert!(
            matches!(err, WorkshopError::AllSourcesFailed { count: 2, .. }),
            "expected AllSourcesFailed, got: {err}"
        );
    }

    /// FailoverIndex reports the number of configured backends.
    #[test]
    fn failover_backend_count() {
        let index = FailoverIndex::new(vec![
            Box::new(MemoryIndex::default()),
            Box::new(MemoryIndex::default()),
        ])
        .unwrap();
        assert_eq!(index.backend_count(), 2);
    }
}
