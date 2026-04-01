// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (c) 2025–present Iron Curtain contributors

//! Content verification — SHA-1 source identification and SHA-256 installed checks.
//!
//! Two distinct hash families serve different roles:
//!
//! - **SHA-1**: used for OpenRA-compatible source identification (IDFiles).
//!   We must match OpenRA's published hashes to correctly identify disc editions,
//!   Steam installs, etc.
//!
//! - **SHA-256**: used for our own installed-content manifest (`content-manifest.toml`).
//!   After content is extracted into managed storage, we hash every file with
//!   SHA-256 for integrity verification, P2P torrent generation, and repair scans.
//!
//! ## Performance features (`fast-verify`)
//!
//! When the `fast-verify` feature is enabled:
//!
//! - **Parallel hashing**: `generate_manifest` and `verify_installed_content` use
//!   rayon to hash multiple files concurrently (~4x speedup on 4+ cores).
//! - **Scratch buffers**: `Sha256Scratch` pre-allocates a reusable read buffer and
//!   hasher instance, eliminating per-file allocation churn during batch verification.
//!   (Per IC distribution analysis §2.5 — ECS Layer 5 zero-allocation pattern.)
//! - **SIMD bitfield**: `VerifyBitfield` tracks pass/fail status of files using
//!   `wide::u64x4` SIMD lanes. Set operations (intersection, union, popcount) are
//!   single-instruction on AVX2/NEON. (Per IC performance doc §2.5.)
//! - **Incremental verification**: `verify_incremental` checks a time-based subset
//!   of files per invocation, spreading I/O load across hours instead of spiking.
//!   (Per IC distribution analysis §2.4 — ECS Layer 4 amortized work pattern.)

use std::collections::BTreeMap;
use std::fs;
use std::io::{self, Read};
use std::path::Path;

use serde::{Deserialize, Serialize};
use thiserror::Error;

use crate::sources::ALL_SOURCES;
use crate::{IdFileCheck, PackageId, SourceId};

/// Schema version for the installed content manifest.
pub const CONTENT_MANIFEST_VERSION: u32 = 1;

/// Installed content manifest — written to `content-manifest.toml` after
/// successful extraction and verification.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct InstalledContentManifest {
    /// Schema version for forward compatibility.
    pub version: u32,
    /// Game identifier (e.g. "ra").
    pub game: String,
    /// Content version tag (e.g. "v1").
    pub content_version: String,
    /// SHA-256 hex digest for each installed file (sorted by path).
    pub files: BTreeMap<String, FileDigest>,
}

/// Per-file digest in the installed content manifest.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct FileDigest {
    /// SHA-256 hex digest (lowercase).
    pub sha256: String,
    /// File size in bytes.
    pub size: u64,
}

/// Errors from verification operations.
#[derive(Debug, Error)]
pub enum VerifyError {
    #[error("I/O error: {0}")]
    Io(#[from] io::Error),
    #[error("SHA-1 mismatch for {path}: expected {expected}, got {actual}")]
    Sha1Mismatch {
        path: String,
        expected: String,
        actual: String,
    },
    #[error("file not found: {0}")]
    FileNotFound(String),
}

/// Checks whether a source root matches a known source by verifying its ID files.
///
/// Returns `Some(source_id)` for the first source whose ID files all match,
/// or `None` if no source matches.
pub fn identify_source(source_root: &Path) -> Option<SourceId> {
    ALL_SOURCES.iter().find_map(|source| {
        let all_match = source
            .id_files
            .iter()
            .all(|check| verify_id_file(source_root, check).unwrap_or(false));
        if all_match {
            Some(source.id)
        } else {
            None
        }
    })
}

/// Verifies a single ID file check against a source root.
///
/// Returns `true` if the file exists and its SHA-1 matches the expected hash.
pub fn verify_id_file(source_root: &Path, check: &IdFileCheck) -> Result<bool, VerifyError> {
    let path = source_root.join(check.path);
    if !path.exists() {
        return Ok(false);
    }

    let hash = sha1_file(&path, check.prefix_length)?;
    Ok(hash == check.sha1)
}

/// Computes the SHA-1 hex digest of a file, optionally reading only the first
/// `prefix_length` bytes.
pub fn sha1_file(path: &Path, prefix_length: Option<u64>) -> Result<String, io::Error> {
    use sha1::{Digest, Sha1};

    let mut file = fs::File::open(path)?;
    let mut hasher = Sha1::new();

    match prefix_length {
        Some(len) => {
            let mut buf = vec![0u8; len as usize];
            file.read_exact(&mut buf)?;
            hasher.update(&buf);
        }
        None => {
            let mut buf = [0u8; 8192];
            loop {
                let n = file.read(&mut buf)?;
                if n == 0 {
                    break;
                }
                hasher.update(buf.get(..n).unwrap_or(&[]));
            }
        }
    }

    Ok(hex_encode(hasher.finalize().as_slice()))
}

/// Computes the SHA-256 hex digest of a file.
pub fn sha256_file(path: &Path) -> Result<String, io::Error> {
    use sha2::{Digest, Sha256};

    let mut file = fs::File::open(path)?;
    let mut hasher = Sha256::new();
    let mut buf = [0u8; 8192];

    loop {
        let n = file.read(&mut buf)?;
        if n == 0 {
            break;
        }
        hasher.update(buf.get(..n).unwrap_or(&[]));
    }

    Ok(hex_encode(hasher.finalize().as_slice()))
}

/// Encodes a byte slice as lowercase hex.
pub(crate) fn hex_encode(bytes: &[u8]) -> String {
    let mut s = String::with_capacity(bytes.len() * 2);
    for b in bytes {
        use std::fmt::Write;
        let _ = write!(s, "{b:02x}");
    }
    s
}

// ── Scratch buffer pattern (IC distribution analysis §2.5) ──────────

/// Pre-allocated scratch space for SHA-256 hashing.
///
/// Eliminates per-file allocation churn during batch verification by reusing
/// both the read buffer and the hasher state. This is the same zero-allocation
/// pattern used by the sim's `TickScratch` — allocate once, `reset()` between
/// uses, never free until the batch is complete.
///
/// ## Usage
///
/// ```rust
/// use cnc_content::verify::Sha256Scratch;
///
/// let tmp = std::env::temp_dir().join("cnc-sha256-scratch-doctest");
/// let _ = std::fs::remove_dir_all(&tmp);
/// std::fs::create_dir_all(&tmp).unwrap();
/// std::fs::write(tmp.join("a.bin"), b"hello").unwrap();
/// std::fs::write(tmp.join("b.bin"), b"world").unwrap();
///
/// let mut scratch = Sha256Scratch::new();
/// let h1 = scratch.hash_file(&tmp.join("a.bin")).unwrap();
/// let h2 = scratch.hash_file(&tmp.join("b.bin")).unwrap();
/// // Different content produces different hashes.
/// assert_ne!(h1, h2);
/// // Same content produces same hash (scratch reuse is correct).
/// let h1b = scratch.hash_file(&tmp.join("a.bin")).unwrap();
/// assert_eq!(h1, h1b);
/// let _ = std::fs::remove_dir_all(&tmp);
/// ```
pub struct Sha256Scratch {
    buffer: Vec<u8>,
    hasher: sha2::Sha256,
}

impl Default for Sha256Scratch {
    fn default() -> Self {
        Self::new()
    }
}

impl Sha256Scratch {
    /// Creates a new scratch with a 64 KB read buffer.
    pub fn new() -> Self {
        Self {
            buffer: vec![0u8; 65536],
            hasher: <sha2::Sha256 as sha2::Digest>::new(),
        }
    }

    /// Hashes a file using the pre-allocated buffer and hasher.
    ///
    /// The hasher is reset before each use — no allocation occurs.
    pub fn hash_file(&mut self, path: &Path) -> Result<String, io::Error> {
        use sha2::Digest;

        self.hasher.reset();
        let mut file = fs::File::open(path)?;

        loop {
            let n = file.read(&mut self.buffer)?;
            if n == 0 {
                break;
            }
            self.hasher.update(&self.buffer[..n]);
        }

        Ok(hex_encode(self.hasher.finalize_reset().as_slice()))
    }
}

// ── Verification functions ──────────────────────────────────────────

/// Verifies all installed content files against a manifest.
///
/// Returns a list of files that are missing or have mismatched hashes.
///
/// When the `fast-verify` feature is enabled, files are hashed in parallel
/// using rayon (one file per thread, up to the rayon thread pool size).
pub fn verify_installed_content(
    content_root: &Path,
    manifest: &InstalledContentManifest,
) -> Vec<String> {
    #[cfg(feature = "fast-verify")]
    {
        verify_installed_content_parallel(content_root, manifest)
    }
    #[cfg(not(feature = "fast-verify"))]
    {
        verify_installed_content_sequential(content_root, manifest)
    }
}

#[cfg(not(feature = "fast-verify"))]
fn verify_installed_content_sequential(
    content_root: &Path,
    manifest: &InstalledContentManifest,
) -> Vec<String> {
    let mut scratch = Sha256Scratch::new();
    let mut failures = Vec::new();

    for (rel_path, expected) in &manifest.files {
        let full_path = content_root.join(rel_path);
        match scratch.hash_file(&full_path) {
            Ok(actual) if actual == expected.sha256 => {}
            _ => failures.push(rel_path.clone()),
        }
    }

    failures
}

#[cfg(feature = "fast-verify")]
fn verify_installed_content_parallel(
    content_root: &Path,
    manifest: &InstalledContentManifest,
) -> Vec<String> {
    use rayon::prelude::*;

    let entries: Vec<_> = manifest.files.iter().collect();

    entries
        .par_iter()
        .filter_map(|(rel_path, expected)| {
            let full_path = content_root.join(rel_path);
            // Each rayon task gets its own scratch — no contention.
            let mut scratch = Sha256Scratch::new();
            match scratch.hash_file(&full_path) {
                Ok(actual) if actual == expected.sha256 => None,
                _ => Some((*rel_path).clone()),
            }
        })
        .collect()
}

/// Generates an installed content manifest by hashing all files under the
/// content root.
///
/// When the `fast-verify` feature is enabled, files are hashed in parallel.
pub fn generate_manifest(
    content_root: &Path,
    game: &str,
    content_version: &str,
    packages: &[PackageId],
) -> Result<InstalledContentManifest, io::Error> {
    // Collect all file paths first.
    let mut paths: Vec<(&str, std::path::PathBuf)> = Vec::new();
    for pkg_id in packages {
        let pkg = crate::package(*pkg_id);
        for test_file in pkg.test_files {
            let full = content_root.join(test_file);
            if full.exists() {
                paths.push((test_file, full));
            }
        }
    }

    #[cfg(feature = "fast-verify")]
    let files = generate_manifest_parallel(&paths)?;

    #[cfg(not(feature = "fast-verify"))]
    let files = generate_manifest_sequential(&paths)?;

    Ok(InstalledContentManifest {
        version: CONTENT_MANIFEST_VERSION,
        game: game.to_string(),
        content_version: content_version.to_string(),
        files,
    })
}

#[cfg(not(feature = "fast-verify"))]
fn generate_manifest_sequential(
    paths: &[(&str, std::path::PathBuf)],
) -> Result<BTreeMap<String, FileDigest>, io::Error> {
    let mut scratch = Sha256Scratch::new();
    let mut files = BTreeMap::new();

    for (test_file, full) in paths {
        let sha256 = scratch.hash_file(full)?;
        let size = fs::metadata(full)?.len();
        files.insert(test_file.to_string(), FileDigest { sha256, size });
    }

    Ok(files)
}

#[cfg(feature = "fast-verify")]
fn generate_manifest_parallel(
    paths: &[(&str, std::path::PathBuf)],
) -> Result<BTreeMap<String, FileDigest>, io::Error> {
    use rayon::prelude::*;

    let results: Vec<Result<(String, FileDigest), io::Error>> = paths
        .par_iter()
        .map(|(test_file, full)| {
            let mut scratch = Sha256Scratch::new();
            let sha256 = scratch.hash_file(full)?;
            let size = fs::metadata(full)?.len();
            Ok((test_file.to_string(), FileDigest { sha256, size }))
        })
        .collect();

    let mut files = BTreeMap::new();
    for result in results {
        let (path, digest) = result?;
        files.insert(path, digest);
    }
    Ok(files)
}

// ── SIMD verification bitfield (IC performance doc §2.5) ────────────

/// SIMD-width bitfield for tracking file verification status.
///
/// Uses `wide::u64x4` (256 bits per SIMD lane) for set operations:
/// - **AND** (intersection): "which files are both installed and verified"
/// - **OR** (union): "which files have been checked at all"
/// - **AND NOT** (difference): "which files still need checking"
/// - **popcount**: "how many files passed/failed"
///
/// Each bit position corresponds to a file index in the manifest.
/// Supports up to 4096 files (16 × u64x4 = 16 × 256 bits). Game content
/// manifests are typically 20–200 files, well within this limit.
///
/// This is the same pattern recommended for P2P piece have/need bitmaps
/// in `p2p-distribute` — the verification bitfield is a natural precursor
/// that exercises the same SIMD codepath.
#[cfg(feature = "fast-verify")]
pub struct VerifyBitfield {
    /// Each `u64x4` holds 256 bits. 16 lanes = 4096 file capacity.
    lanes: [wide::u64x4; 16],
    /// Number of files tracked.
    len: usize,
}

#[cfg(feature = "fast-verify")]
impl VerifyBitfield {
    /// Maximum number of files supported.
    pub const MAX_FILES: usize = 16 * 256;

    /// Creates a new bitfield with all bits cleared (all files unverified).
    pub fn new(file_count: usize) -> Self {
        assert!(
            file_count <= Self::MAX_FILES,
            "VerifyBitfield supports up to {} files, got {file_count}",
            Self::MAX_FILES
        );
        Self {
            lanes: [wide::u64x4::ZERO; 16],
            len: file_count,
        }
    }

    /// Marks a file index as set (verified/passed).
    pub fn set(&mut self, index: usize) {
        debug_assert!(index < self.len);
        let lane = index / 256;
        let bit_in_lane = index % 256;
        let word = bit_in_lane / 64;
        let bit = bit_in_lane % 64;

        let mut arr = self.lanes[lane].to_array();
        arr[word] |= 1u64 << bit;
        self.lanes[lane] = wide::u64x4::from(arr);
    }

    /// Returns `true` if the given file index is set.
    pub fn get(&self, index: usize) -> bool {
        debug_assert!(index < self.len);
        let lane = index / 256;
        let bit_in_lane = index % 256;
        let word = bit_in_lane / 64;
        let bit = bit_in_lane % 64;

        let arr = self.lanes[lane].to_array();
        arr[word] & (1u64 << bit) != 0
    }

    /// Returns the number of set bits (files that passed verification).
    pub fn count_ones(&self) -> usize {
        let mut total = 0usize;
        for lane in &self.lanes {
            for word in lane.to_array() {
                total += word.count_ones() as usize;
            }
        }
        total
    }

    /// Returns the number of files tracked.
    pub fn len(&self) -> usize {
        self.len
    }

    /// Returns `true` if no files are tracked.
    pub fn is_empty(&self) -> bool {
        self.len == 0
    }

    /// Returns the number of files that failed (not set).
    pub fn count_failures(&self) -> usize {
        self.len - self.count_ones()
    }

    /// SIMD AND — intersection of two bitfields.
    ///
    /// Returns a new bitfield where only bits set in *both* inputs are set.
    /// Useful for "which files are both installed AND verified?"
    pub fn and(&self, other: &Self) -> Self {
        let mut result = Self::new(self.len.max(other.len));
        for i in 0..16 {
            result.lanes[i] = self.lanes[i] & other.lanes[i];
        }
        result
    }

    /// SIMD OR — union of two bitfields.
    ///
    /// Returns a new bitfield where bits set in *either* input are set.
    /// Useful for "which files have been checked at all?"
    pub fn or(&self, other: &Self) -> Self {
        let mut result = Self::new(self.len.max(other.len));
        for i in 0..16 {
            result.lanes[i] = self.lanes[i] | other.lanes[i];
        }
        result
    }

    /// SIMD AND NOT — difference: bits set in `self` but not in `other`.
    ///
    /// Useful for "which files still need checking?" (all AND NOT checked).
    pub fn and_not(&self, other: &Self) -> Self {
        let mut result = Self::new(self.len.max(other.len));
        for i in 0..16 {
            result.lanes[i] = self.lanes[i] & !other.lanes[i];
        }
        result
    }

    /// Returns indices of all set bits.
    pub fn set_indices(&self) -> Vec<usize> {
        let mut indices = Vec::new();
        for (lane_idx, lane) in self.lanes.iter().enumerate() {
            for (word_idx, word) in lane.to_array().iter().enumerate() {
                let mut w = *word;
                while w != 0 {
                    let bit = w.trailing_zeros() as usize;
                    let index = lane_idx * 256 + word_idx * 64 + bit;
                    if index < self.len {
                        indices.push(index);
                    }
                    w &= w - 1; // clear lowest set bit
                }
            }
        }
        indices
    }
}

// ── Incremental/staggered verification (IC distribution analysis §2.4) ─

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

#[cfg(test)]
mod tests;
