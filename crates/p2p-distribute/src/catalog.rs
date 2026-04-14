// SPDX-License-Identifier: MIT OR Apache-2.0

//! Catalog sync planning — computes download/delete actions from manifest diffs.
//!
//! Given a mirror's local content state and a remote [`GroupManifest`], the
//! [`plan_sync`] function produces a [`SyncPlan`] listing exactly what the
//! mirror must do to converge with the master's catalog. The plan is pure
//! data — the caller decides how to execute it (queue downloads via
//! [`crate::session_manager::DownloadSession`], delete files, etc.).
//!
//! ## Replication policies
//!
//! [`ReplicationPolicy`] controls which entries a mirror replicates:
//!
//! - **Full** — replicate everything in the manifest.
//! - **PrefixFilter** — replicate only entries whose paths match one of the
//!   specified prefixes (e.g. `["maps/", "movies/"]`). Useful for geography-
//!   based sharding or partial mirrors that only serve specific content types.

use std::collections::{HashMap, HashSet};

use crate::manifest::{ContentEntry, GroupManifest};

// ── Sync actions ────────────────────────────────────────────────────

/// An action a mirror node must take to synchronize with a manifest.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum SyncAction {
    /// Download a new or modified file.
    Download {
        /// The content entry to download.
        entry: ContentEntry,
    },
    /// Delete a local file that was removed from the manifest.
    Delete {
        /// Relative path of the file to delete.
        path: String,
    },
}

// ── Replication policy ──────────────────────────────────────────────

/// Controls which content entries a mirror replicates.
///
/// Full mirrors replicate the entire catalog. Partial mirrors use
/// [`PrefixFilter`](Self::PrefixFilter) to replicate only a subset,
/// which is useful for large catalogs where mirrors specialize by
/// content type or geographic region.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ReplicationPolicy {
    /// Replicate all entries in the manifest.
    Full,
    /// Replicate only entries whose paths start with one of the given prefixes.
    ///
    /// Example: `["maps/", "movies/"]` replicates only files under `maps/`
    /// and `movies/`, ignoring everything else.
    PrefixFilter {
        /// Path prefixes to include (forward slashes, no leading `/`).
        prefixes: Vec<String>,
    },
}

impl ReplicationPolicy {
    /// Returns `true` if the given path is included by this policy.
    pub fn matches(&self, path: &str) -> bool {
        match self {
            Self::Full => true,
            Self::PrefixFilter { prefixes } => {
                prefixes.iter().any(|p| path.starts_with(p.as_str()))
            }
        }
    }
}

// ── Sync plan ───────────────────────────────────────────────────────

/// A computed plan of actions to synchronize a mirror with a manifest.
///
/// Produced by [`plan_sync`]. The caller iterates [`actions`](Self::actions)
/// and executes each one (queue downloads, delete files). Summary fields
/// provide totals for progress reporting.
#[derive(Debug, Clone)]
pub struct SyncPlan {
    /// Ordered list of sync actions.
    pub actions: Vec<SyncAction>,
    /// Total bytes to download (sum of all Download entry sizes).
    pub bytes_to_download: u64,
    /// Number of files to download (new + modified).
    pub files_to_download: u32,
    /// Number of files to delete (removed from manifest).
    pub files_to_delete: u32,
}

impl SyncPlan {
    /// Returns `true` if no actions are needed (mirror is already in sync).
    pub fn is_empty(&self) -> bool {
        self.actions.is_empty()
    }
}

// ── Plan computation ────────────────────────────────────────────────

/// Computes the sync plan to bring a mirror's local state in line with a
/// remote manifest.
///
/// ## Algorithm
///
/// 1. Build a HashMap of local entries keyed by path.
/// 2. For each remote entry matching the replication policy:
///    - If not present locally → Download.
///    - If present but hash differs → Download (re-fetch modified file).
///    - If present and hash matches → no action (already in sync).
/// 3. For each local entry not present in the remote manifest → Delete.
///
/// ## Parameters
///
/// - `local_entries`: the mirror's current content (from a filesystem scan
///   or a previously applied manifest). Need not be sorted.
/// - `remote`: the master's latest manifest.
/// - `policy`: which entries to replicate.
///
/// ## Example
///
/// ```
/// # use p2p_distribute::manifest::{ContentEntry, GroupManifest};
/// # use p2p_distribute::catalog::{plan_sync, ReplicationPolicy, SyncAction};
/// # use p2p_distribute::NetworkId;
/// let local = vec![
///     ContentEntry::new("old.bin", [0xAA; 32], 100).unwrap(),
/// ];
/// let remote = GroupManifest::builder(NetworkId::TEST)
///     .version(2)
///     .add_entry(ContentEntry::new("new.bin", [0xBB; 32], 200).unwrap())
///     .build().unwrap();
///
/// let plan = plan_sync(&local, &remote, &ReplicationPolicy::Full);
/// assert_eq!(plan.files_to_download, 1); // new.bin
/// assert_eq!(plan.files_to_delete, 1);   // old.bin
/// ```
pub fn plan_sync(
    local_entries: &[ContentEntry],
    remote: &GroupManifest,
    policy: &ReplicationPolicy,
) -> SyncPlan {
    let mut actions = Vec::new();
    let mut bytes_to_download: u64 = 0;
    let mut files_to_download: u32 = 0;
    let mut files_to_delete: u32 = 0;

    // Index local entries by path for O(1) lookup.
    let local_map: HashMap<&str, &ContentEntry> =
        local_entries.iter().map(|e| (e.path(), e)).collect();

    // ── Forward pass: check each remote entry against local state ────
    for remote_entry in remote.entries() {
        if !policy.matches(remote_entry.path()) {
            continue;
        }

        match local_map.get(remote_entry.path()) {
            None => {
                // New file — not present locally.
                bytes_to_download = bytes_to_download.saturating_add(remote_entry.file_size());
                files_to_download = files_to_download.saturating_add(1);
                actions.push(SyncAction::Download {
                    entry: remote_entry.clone(),
                });
            }
            Some(local) => {
                if local.content_hash() != remote_entry.content_hash() {
                    // Modified — hash changed, re-download.
                    bytes_to_download = bytes_to_download.saturating_add(remote_entry.file_size());
                    files_to_download = files_to_download.saturating_add(1);
                    actions.push(SyncAction::Download {
                        entry: remote_entry.clone(),
                    });
                }
                // Unchanged — no action.
            }
        }
    }

    // ── Reverse pass: find local files removed from the manifest ─────
    //
    // Only delete files that the policy would have replicated. A partial
    // mirror should not delete files outside its prefix filter.
    let remote_paths: HashSet<&str> = remote.entries().iter().map(|e| e.path()).collect();

    for local in local_entries {
        if policy.matches(local.path()) && !remote_paths.contains(local.path()) {
            files_to_delete = files_to_delete.saturating_add(1);
            actions.push(SyncAction::Delete {
                path: local.path().to_owned(),
            });
        }
    }

    SyncPlan {
        actions,
        bytes_to_download,
        files_to_download,
        files_to_delete,
    }
}

// ── Tests ───────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;
    use crate::network_id::NetworkId;

    /// Helper: creates a content entry with a fill-byte hash.
    fn entry(path: &str, hash_byte: u8, size: u64) -> ContentEntry {
        ContentEntry::new(path, [hash_byte; 32], size).unwrap()
    }

    /// Helper: builds a manifest from entries.
    fn manifest(entries: Vec<ContentEntry>) -> GroupManifest {
        let mut builder = GroupManifest::builder(NetworkId::TEST).version(1);
        for e in entries {
            builder = builder.add_entry(e);
        }
        builder.build().unwrap()
    }

    // ── ReplicationPolicy ───────────────────────────────────────────

    /// Full policy matches everything.
    #[test]
    fn full_policy_matches_all() {
        let policy = ReplicationPolicy::Full;
        assert!(policy.matches("any/path/here.bin"));
        assert!(policy.matches(""));
    }

    /// PrefixFilter matches only paths under the specified prefixes.
    #[test]
    fn prefix_filter_matches_correctly() {
        let policy = ReplicationPolicy::PrefixFilter {
            prefixes: vec!["maps/".to_owned(), "movies/".to_owned()],
        };
        assert!(policy.matches("maps/map1.mix"));
        assert!(policy.matches("movies/intro.vqa"));
        assert!(!policy.matches("music/track1.aud"));
        assert!(!policy.matches("readme.txt"));
    }

    // ── plan_sync ───────────────────────────────────────────────────

    /// Empty local + full remote → all files are downloads.
    ///
    /// A fresh mirror with no content should download everything.
    #[test]
    fn plan_sync_empty_local() {
        let remote = manifest(vec![entry("a.bin", 0xAA, 100), entry("b.bin", 0xBB, 200)]);
        let plan = plan_sync(&[], &remote, &ReplicationPolicy::Full);

        assert_eq!(plan.files_to_download, 2);
        assert_eq!(plan.bytes_to_download, 300);
        assert_eq!(plan.files_to_delete, 0);
    }

    /// Identical content → no actions needed.
    ///
    /// When local and remote match perfectly, the mirror is already in sync.
    #[test]
    fn plan_sync_already_in_sync() {
        let entries = vec![entry("a.bin", 0xAA, 100)];
        let remote = manifest(entries.clone());
        let plan = plan_sync(&entries, &remote, &ReplicationPolicy::Full);

        assert!(plan.is_empty());
        assert_eq!(plan.files_to_download, 0);
        assert_eq!(plan.files_to_delete, 0);
    }

    /// Remote has new files → downloads, no deletes.
    #[test]
    fn plan_sync_new_files() {
        let local = vec![entry("existing.bin", 0xAA, 100)];
        let remote = manifest(vec![
            entry("existing.bin", 0xAA, 100),
            entry("new.bin", 0xBB, 500),
        ]);
        let plan = plan_sync(&local, &remote, &ReplicationPolicy::Full);

        assert_eq!(plan.files_to_download, 1);
        assert_eq!(plan.bytes_to_download, 500);
        assert_eq!(plan.files_to_delete, 0);
    }

    /// Remote removed files → deletes, no downloads.
    #[test]
    fn plan_sync_removed_files() {
        let local = vec![entry("keep.bin", 0xAA, 100), entry("remove.bin", 0xBB, 200)];
        let remote = manifest(vec![entry("keep.bin", 0xAA, 100)]);
        let plan = plan_sync(&local, &remote, &ReplicationPolicy::Full);

        assert_eq!(plan.files_to_download, 0);
        assert_eq!(plan.files_to_delete, 1);
        assert!(plan
            .actions
            .iter()
            .any(|a| matches!(a, SyncAction::Delete { path } if path == "remove.bin")));
    }

    /// Modified file (different hash) → re-download.
    #[test]
    fn plan_sync_modified_file() {
        let local = vec![entry("data.bin", 0xAA, 100)];
        let remote = manifest(vec![entry("data.bin", 0xBB, 200)]);
        let plan = plan_sync(&local, &remote, &ReplicationPolicy::Full);

        assert_eq!(plan.files_to_download, 1);
        assert_eq!(plan.bytes_to_download, 200);
        assert_eq!(plan.files_to_delete, 0);
    }

    /// Mixed adds, removes, and modifications.
    #[test]
    fn plan_sync_mixed_changes() {
        let local = vec![
            entry("keep.bin", 0xAA, 100),   // unchanged
            entry("modify.bin", 0xBB, 200), // will change
            entry("delete.bin", 0xCC, 300), // will be removed
        ];
        let remote = manifest(vec![
            entry("keep.bin", 0xAA, 100),   // unchanged
            entry("modify.bin", 0xFF, 999), // modified
            entry("new.bin", 0xDD, 400),    // added
        ]);
        let plan = plan_sync(&local, &remote, &ReplicationPolicy::Full);

        assert_eq!(plan.files_to_download, 2); // modify + new
        assert_eq!(plan.bytes_to_download, 999 + 400);
        assert_eq!(plan.files_to_delete, 1); // delete
    }

    /// PrefixFilter only downloads matching entries and only deletes
    /// matching local files.
    ///
    /// A partial mirror with prefix "maps/" should ignore files outside
    /// that prefix entirely — don't download them, don't delete them.
    #[test]
    fn plan_sync_with_prefix_filter() {
        let local = vec![
            entry("maps/map1.mix", 0xAA, 100),
            entry("maps/map2.mix", 0xBB, 200), // will be removed
        ];
        let remote = manifest(vec![
            entry("maps/map1.mix", 0xAA, 100),    // unchanged
            entry("movies/intro.vqa", 0xCC, 500), // outside prefix
        ]);
        let policy = ReplicationPolicy::PrefixFilter {
            prefixes: vec!["maps/".to_owned()],
        };
        let plan = plan_sync(&local, &remote, &policy);

        // maps/map2.mix was removed from remote → delete.
        assert_eq!(plan.files_to_delete, 1);
        // movies/intro.vqa is outside the prefix → not downloaded.
        assert_eq!(plan.files_to_download, 0);
    }

    /// Both empty → no actions.
    #[test]
    fn plan_sync_both_empty() {
        let remote = manifest(vec![]);
        let plan = plan_sync(&[], &remote, &ReplicationPolicy::Full);
        assert!(plan.is_empty());
    }

    /// SyncPlan::is_empty reflects action count.
    #[test]
    fn sync_plan_is_empty_reflects_actions() {
        let plan = SyncPlan {
            actions: vec![],
            bytes_to_download: 0,
            files_to_download: 0,
            files_to_delete: 0,
        };
        assert!(plan.is_empty());

        let plan_with_actions = SyncPlan {
            actions: vec![SyncAction::Delete {
                path: "x".to_owned(),
            }],
            bytes_to_download: 0,
            files_to_download: 0,
            files_to_delete: 1,
        };
        assert!(!plan_with_actions.is_empty());
    }
}
