// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (c) 2025–present Iron Curtain contributors

//! Incremental/staggered verification — checks a time-based subset of files per
//! invocation, spreading I/O load across hours instead of spiking it.
//!
//! Split from `verify/mod.rs` because the incremental strategy is a self-contained
//! concern (IC distribution analysis §2.4) that can be understood and tested
//! independently of the core SHA-based verification functions.
//!
//! ## Strategy (IC distribution analysis §2.4)
//!
//! Divide the manifest files into `num_slots` groups. Each call checks only
//! `file_index % num_slots == slot`. Rotating `slot = current_hour % num_slots`
//! distributes I/O uniformly across hours — no single hour spikes.

use std::path::Path;

use super::{InstalledContentManifest, Sha256Scratch};

/// Result of an incremental verification pass.
#[derive(Debug, Clone)]
pub struct IncrementalVerifyResult {
    /// Files that were checked in this pass.
    pub checked: Vec<String>,
    /// Files that failed verification (subset of `checked`).
    pub failures: Vec<String>,
    /// Total files in the manifest.
    pub total_files: usize,
    /// The slot index used for this pass (0..num_slots).
    pub slot: usize,
    /// Total number of slots.
    pub num_slots: usize,
}

/// Verifies a time-based subset of installed content.
///
/// Instead of checking all files at once (which spikes I/O), this function
/// divides files into `num_slots` groups and checks only the group matching
/// `slot`. Call with `slot = current_hour % num_slots` to spread verification
/// across hours.
///
/// Per IC distribution analysis §2.4 (ECS Layer 4 — amortized work):
/// "Instead of checking all 50 subscribed resources at once every 24 hours,
/// check `resource_index % 24 == current_hour`."
///
/// ## Example
///
/// ```rust
/// use cnc_content::verify::{
///     verify_incremental, InstalledContentManifest, FileDigest, Sha256Scratch,
/// };
/// use std::collections::BTreeMap;
///
/// let tmp = std::env::temp_dir().join("cnc-verify-incr-doctest");
/// let _ = std::fs::remove_dir_all(&tmp);
/// std::fs::create_dir_all(&tmp).unwrap();
/// std::fs::write(tmp.join("a.mix"), b"data-a").unwrap();
/// std::fs::write(tmp.join("b.mix"), b"data-b").unwrap();
///
/// // Build a manifest with correct hashes.
/// let mut scratch = Sha256Scratch::new();
/// let mut files = BTreeMap::new();
/// files.insert("a.mix".to_string(), FileDigest {
///     sha256: scratch.hash_file(&tmp.join("a.mix")).unwrap(),
///     size: 6,
/// });
/// files.insert("b.mix".to_string(), FileDigest {
///     sha256: scratch.hash_file(&tmp.join("b.mix")).unwrap(),
///     size: 6,
/// });
/// let manifest = InstalledContentManifest {
///     version: 1, game: "ra".into(), content_version: "v1".into(), files,
/// };
///
/// // Slot 0 of 2 checks ~half the files.
/// let result = verify_incremental(&tmp, &manifest, 0, 2);
/// assert!(result.failures.is_empty());
/// let _ = std::fs::remove_dir_all(&tmp);
/// ```
pub fn verify_incremental(
    content_root: &Path,
    manifest: &InstalledContentManifest,
    slot: usize,
    num_slots: usize,
) -> IncrementalVerifyResult {
    let entries: Vec<_> = manifest.files.iter().collect();
    let total_files = entries.len();

    // Select files for this slot: file_index % num_slots == slot
    let slot_entries: Vec<_> = entries
        .iter()
        .enumerate()
        .filter(|(i, _)| *i % num_slots == slot % num_slots)
        .map(|(_, entry)| *entry)
        .collect();

    let mut scratch = Sha256Scratch::new();
    let mut checked = Vec::new();
    let mut failures = Vec::new();

    for (rel_path, expected) in slot_entries {
        checked.push(rel_path.clone());
        let full_path = content_root.join(rel_path);
        match scratch.hash_file(&full_path) {
            Ok(actual) if actual == expected.sha256 => {}
            _ => failures.push(rel_path.clone()),
        }
    }

    IncrementalVerifyResult {
        checked,
        failures,
        total_files,
        slot: slot % num_slots,
        num_slots,
    }
}
