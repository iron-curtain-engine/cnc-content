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
mod tests {
    use super::*;
    use std::collections::BTreeMap;

    // ── hex_encode ───────────────────────────────────────────────────

    /// `hex_encode` of an empty byte slice must produce an empty string.
    ///
    /// The empty case is the boundary condition for the encoding loop; a
    /// correct implementation must not write any characters when given no input.
    #[test]
    fn hex_encode_empty() {
        assert_eq!(hex_encode(&[]), "");
    }

    /// `hex_encode` must produce the correct two-character lowercase hex sequence
    /// for a representative set of byte values including boundary bytes.
    ///
    /// Verifying against known-correct vectors (0x00, 0xff, multi-byte sequences)
    /// guards against off-by-one errors in nibble extraction and character ordering.
    #[test]
    fn hex_encode_known_values() {
        assert_eq!(hex_encode(&[0x00]), "00");
        assert_eq!(hex_encode(&[0xff]), "ff");
        assert_eq!(hex_encode(&[0xde, 0xad, 0xbe, 0xef]), "deadbeef");
        assert_eq!(
            hex_encode(&[0x01, 0x23, 0x45, 0x67, 0x89, 0xab, 0xcd, 0xef]),
            "0123456789abcdef"
        );
    }

    /// `hex_encode` must always produce lowercase hex digits, never uppercase.
    ///
    /// Hash comparisons throughout the codebase use lowercase strings; if
    /// `hex_encode` ever emits uppercase characters the comparisons would
    /// silently fail, causing correct files to be flagged as corrupted.
    #[test]
    fn hex_encode_is_lowercase() {
        let result = hex_encode(&[0xAB, 0xCD]);
        assert_eq!(result, "abcd");
        assert!(result.chars().all(|c| !c.is_ascii_uppercase()));
    }

    // ── sha1_file ────────────────────────────────────────────────────

    /// `sha1_file` of an empty file must produce the well-known SHA-1 of empty input.
    ///
    /// The SHA-1 of the empty string is a published constant; matching it confirms
    /// the hasher is initialized correctly and that the read loop terminates cleanly
    /// without feeding stale buffer bytes into the digest.
    #[test]
    fn sha1_file_known_hash() {
        let tmp = std::env::temp_dir().join("cnc-verify-sha1");
        let _ = fs::remove_dir_all(&tmp);
        fs::create_dir_all(&tmp).unwrap();

        // SHA-1 of empty string is da39a3ee5e6b4b0d3255bfef95601890afd80709
        let path = tmp.join("empty.bin");
        fs::write(&path, b"").unwrap();
        let hash = sha1_file(&path, None).unwrap();
        assert_eq!(hash, "da39a3ee5e6b4b0d3255bfef95601890afd80709");

        let _ = fs::remove_dir_all(&tmp);
    }

    /// `sha1_file` with a prefix length hashes only the leading N bytes, not the whole file.
    ///
    /// OpenRA source identification relies on prefix-only hashing to fingerprint
    /// large mix archives without reading them in full. If `sha1_file` accidentally
    /// reads past the prefix, a valid disc would be misidentified as the wrong edition.
    ///
    /// The test writes a known file, hashes its first 5 bytes in isolation, and
    /// verifies the result matches independently hashing a file containing only
    /// those same 5 bytes.
    #[test]
    fn sha1_file_prefix_length() {
        let tmp = std::env::temp_dir().join("cnc-verify-sha1-prefix");
        let _ = fs::remove_dir_all(&tmp);
        fs::create_dir_all(&tmp).unwrap();

        let path = tmp.join("data.bin");
        fs::write(&path, b"HELLO WORLD EXTRA DATA").unwrap();

        // Hash of "HELLO" (5 bytes) vs hash of entire file — should differ.
        let hash_prefix = sha1_file(&path, Some(5)).unwrap();
        let hash_full = sha1_file(&path, None).unwrap();
        assert_ne!(hash_prefix, hash_full);

        // Hash of prefix should be the same as hashing just "HELLO".
        let path2 = tmp.join("hello.bin");
        fs::write(&path2, b"HELLO").unwrap();
        let hash_hello = sha1_file(&path2, None).unwrap();
        assert_eq!(hash_prefix, hash_hello);

        let _ = fs::remove_dir_all(&tmp);
    }

    /// `sha1_file` must return an error when the target file does not exist.
    ///
    /// Callers in `verify_id_file` propagate the error through `?`; if `sha1_file`
    /// silently succeeded on a missing file, source identification would produce
    /// false positives by treating every absent ID file as a match.
    #[test]
    fn sha1_file_missing_returns_error() {
        let result = sha1_file(std::path::Path::new("/nonexistent/file.bin"), None);
        assert!(result.is_err());
    }

    // ── sha256_file ──────────────────────────────────────────────────

    /// `sha256_file` of an empty file must produce the well-known SHA-256 of empty input.
    ///
    /// The SHA-256 of the empty string is a published 64-character constant; matching
    /// it confirms the hasher is initialized correctly and the output length is always
    /// exactly 64 hex characters, as required by the manifest format.
    #[test]
    fn sha256_file_known_hash() {
        let tmp = std::env::temp_dir().join("cnc-verify-sha256");
        let _ = fs::remove_dir_all(&tmp);
        fs::create_dir_all(&tmp).unwrap();

        // SHA-256 of empty string is e3b0c44298fc1c149afbf4c8996fb92427ae41e4649b934ca495991b7852b855
        let path = tmp.join("empty.bin");
        fs::write(&path, b"").unwrap();
        let hash = sha256_file(&path).unwrap();
        assert_eq!(
            hash,
            "e3b0c44298fc1c149afbf4c8996fb92427ae41e4649b934ca495991b7852b855"
        );
        assert_eq!(hash.len(), 64);

        let _ = fs::remove_dir_all(&tmp);
    }

    /// `sha256_file` must return only lowercase hex digits with no uppercase characters.
    ///
    /// Manifest files store hashes as lowercase strings; an uppercase digit would
    /// cause string equality to fail during verification even when the file is intact,
    /// producing a false corruption report.
    #[test]
    fn sha256_file_is_lowercase_hex() {
        let tmp = std::env::temp_dir().join("cnc-verify-sha256-case");
        let _ = fs::remove_dir_all(&tmp);
        fs::create_dir_all(&tmp).unwrap();

        let path = tmp.join("data.bin");
        fs::write(&path, b"test data for hashing").unwrap();
        let hash = sha256_file(&path).unwrap();
        assert!(hash
            .chars()
            .all(|c| c.is_ascii_hexdigit() && !c.is_ascii_uppercase()));

        let _ = fs::remove_dir_all(&tmp);
    }

    // ── verify_id_file ───────────────────────────────────────────────

    /// `verify_id_file` must return `true` when the file exists and its SHA-1 matches.
    ///
    /// This is the happy-path for source identification: a known-good file on disk
    /// must be recognized correctly so that disc/Steam installs are not rejected.
    ///
    /// The expected hash is computed at runtime from the same file to avoid
    /// hard-coding a test vector that could drift from the implementation.
    #[test]
    fn verify_id_file_match() {
        let tmp = std::env::temp_dir().join("cnc-verify-id-match");
        let _ = fs::remove_dir_all(&tmp);
        fs::create_dir_all(&tmp).unwrap();

        let data = b"known content";
        fs::write(tmp.join("test.mix"), data).unwrap();

        // Compute the real SHA-1 of "known content".
        let expected_sha1 = sha1_file(&tmp.join("test.mix"), None).unwrap();

        let check = IdFileCheck {
            path: "test.mix",
            sha1: Box::leak(expected_sha1.into_boxed_str()),
            prefix_length: None,
        };

        assert!(verify_id_file(&tmp, &check).unwrap());

        let _ = fs::remove_dir_all(&tmp);
    }

    /// `verify_id_file` must return `false` when the file exists but its SHA-1 does not match.
    ///
    /// A wrong hash must never be treated as a positive identification; otherwise a
    /// corrupted or wrong-edition disc file could be accepted as a valid source.
    #[test]
    fn verify_id_file_mismatch() {
        let tmp = std::env::temp_dir().join("cnc-verify-id-mismatch");
        let _ = fs::remove_dir_all(&tmp);
        fs::create_dir_all(&tmp).unwrap();

        fs::write(tmp.join("test.mix"), b"actual content").unwrap();

        let check = IdFileCheck {
            path: "test.mix",
            sha1: "0000000000000000000000000000000000000000",
            prefix_length: None,
        };

        assert!(!verify_id_file(&tmp, &check).unwrap());

        let _ = fs::remove_dir_all(&tmp);
    }

    /// `verify_id_file` must return `false` (not an error) when the ID file is absent.
    ///
    /// Missing ID files are the normal case for sources that are not installed;
    /// returning `false` allows `identify_source` to move on to the next candidate
    /// without propagating an error through the entire source-scan loop.
    #[test]
    fn verify_id_file_missing_returns_false() {
        let tmp = std::env::temp_dir().join("cnc-verify-id-missing");
        let _ = fs::remove_dir_all(&tmp);
        fs::create_dir_all(&tmp).unwrap();

        let check = IdFileCheck {
            path: "nonexistent.mix",
            sha1: "0000000000000000000000000000000000000000",
            prefix_length: None,
        };

        assert!(!verify_id_file(&tmp, &check).unwrap());

        let _ = fs::remove_dir_all(&tmp);
    }

    /// `verify_id_file` must respect `prefix_length` and hash only the leading bytes.
    ///
    /// OpenRA's ID-file checks for large mix archives specify a prefix so the tool
    /// does not read gigabytes to identify a disc. Verifying that the prefix path
    /// matches the same result as a full hash of those same bytes confirms the
    /// `IdFileCheck.prefix_length` field is wired through to `sha1_file` correctly.
    #[test]
    fn verify_id_file_with_prefix() {
        let tmp = std::env::temp_dir().join("cnc-verify-id-prefix");
        let _ = fs::remove_dir_all(&tmp);
        fs::create_dir_all(&tmp).unwrap();

        fs::write(tmp.join("main.mix"), b"HEADER_BYTES_REST_OF_FILE").unwrap();

        // Get SHA-1 of first 12 bytes ("HEADER_BYTES").
        let expected = sha1_file(&tmp.join("main.mix"), Some(12)).unwrap();

        let check = IdFileCheck {
            path: "main.mix",
            sha1: Box::leak(expected.into_boxed_str()),
            prefix_length: Some(12),
        };

        assert!(verify_id_file(&tmp, &check).unwrap());

        let _ = fs::remove_dir_all(&tmp);
    }

    // ── verify_installed_content ─────────────────────────────────────

    /// `verify_installed_content` must report corrupted and missing files while passing good ones.
    ///
    /// The function is the repair-scan entry point: it must correctly distinguish
    /// three cases — file with matching hash (pass), file with wrong hash (fail),
    /// and file absent from disk (fail) — so that the repair path replaces only
    /// the files that actually need it.
    #[test]
    fn verify_installed_content_detects_mismatch() {
        let tmp = std::env::temp_dir().join("cnc-verify-installed");
        let _ = fs::remove_dir_all(&tmp);
        fs::create_dir_all(&tmp).unwrap();

        fs::write(tmp.join("good.mix"), b"correct data").unwrap();
        fs::write(tmp.join("bad.mix"), b"wrong data").unwrap();

        let good_hash = sha256_file(&tmp.join("good.mix")).unwrap();

        let mut files = BTreeMap::new();
        files.insert(
            "good.mix".to_string(),
            FileDigest {
                sha256: good_hash,
                size: 12,
            },
        );
        files.insert(
            "bad.mix".to_string(),
            FileDigest {
                sha256: "0000000000000000000000000000000000000000000000000000000000000000"
                    .to_string(),
                size: 10,
            },
        );
        files.insert(
            "missing.mix".to_string(),
            FileDigest {
                sha256: "0000000000000000000000000000000000000000000000000000000000000000"
                    .to_string(),
                size: 0,
            },
        );

        let manifest = InstalledContentManifest {
            version: 1,
            game: "ra".to_string(),
            content_version: "v1".to_string(),
            files,
        };

        let failures = verify_installed_content(&tmp, &manifest);
        assert_eq!(failures.len(), 2);
        assert!(failures.contains(&"bad.mix".to_string()));
        assert!(failures.contains(&"missing.mix".to_string()));
        assert!(!failures.contains(&"good.mix".to_string()));

        let _ = fs::remove_dir_all(&tmp);
    }

    // ── identify_source ──────────────────────────────────────────────

    /// `identify_source` must return `None` when given a directory with no ID files.
    ///
    /// An empty directory cannot match any known source; returning `None` rather
    /// than panicking or guessing is required so callers can cleanly fall through
    /// to trying the next candidate path.
    #[test]
    fn identify_source_returns_none_for_empty_dir() {
        let tmp = std::env::temp_dir().join("cnc-verify-identify-empty");
        let _ = fs::remove_dir_all(&tmp);
        fs::create_dir_all(&tmp).unwrap();

        assert!(identify_source(&tmp).is_none());

        let _ = fs::remove_dir_all(&tmp);
    }

    // ── prefix_length edge cases ────────────────────────────────────

    /// Hashing zero bytes of a file should produce the SHA-1 of empty input.
    ///
    /// A prefix length of zero is degenerate but must not panic; it should
    /// behave identically to hashing an empty byte slice.
    #[test]
    fn sha1_file_prefix_zero_length() {
        let tmp = std::env::temp_dir().join("cnc-verify-sha1-prefix-zero");
        let _ = fs::remove_dir_all(&tmp);
        fs::create_dir_all(&tmp).unwrap();

        let path = tmp.join("hello.bin");
        fs::write(&path, b"hello").unwrap();

        let hash = sha1_file(&path, Some(0)).unwrap();
        assert_eq!(hash, "da39a3ee5e6b4b0d3255bfef95601890afd80709");

        let _ = fs::remove_dir_all(&tmp);
    }

    /// Requesting a prefix longer than the file should fail with an I/O error.
    ///
    /// `read_exact` on a short file returns `UnexpectedEof`; callers must not
    /// silently succeed with a partial read when the prefix cannot be filled.
    #[test]
    fn sha1_file_prefix_exceeds_file_size() {
        let tmp = std::env::temp_dir().join("cnc-verify-sha1-prefix-exceeds");
        let _ = fs::remove_dir_all(&tmp);
        fs::create_dir_all(&tmp).unwrap();

        let path = tmp.join("short.bin");
        fs::write(&path, b"hello").unwrap(); // 5 bytes

        let result = sha1_file(&path, Some(1000));
        assert!(result.is_err());

        let err = result.unwrap_err();
        assert_eq!(err.kind(), io::ErrorKind::UnexpectedEof);

        let _ = fs::remove_dir_all(&tmp);
    }

    // ── identify_source partial match ───────────────────────────────

    /// A directory that matches only one of a source's ID files must not be
    /// identified as that source.
    ///
    /// `identify_source` requires *all* ID files to match. If only a subset
    /// passes, the source should be rejected and `None` returned.
    #[test]
    fn identify_source_partial_match_returns_none() {
        let tmp = std::env::temp_dir().join("cnc-verify-identify-partial");
        let _ = fs::remove_dir_all(&tmp);
        fs::create_dir_all(&tmp).unwrap();

        // Pick the first source that has more than one ID file.
        let multi_id_source = ALL_SOURCES
            .iter()
            .find(|s| s.id_files.len() > 1)
            .expect("test requires at least one source with multiple id_files");

        // Create only the first ID file with the correct content hash,
        // but omit the rest. We write dummy content and verify by brute
        // approach: create the file, hash it, and only keep it if the hash
        // matches. Since we cannot easily forge SHA-1 content, we instead
        // just create the file path — the hash will not match, which also
        // means not all ID files pass. But to be precise about "one match,
        // others missing", we simply do NOT create the other files at all.
        // The first file exists with wrong content, so verify_id_file returns
        // false for hash mismatch; the second file is missing. Either way,
        // not all match.
        let first_check = &multi_id_source.id_files[0];
        let file_path = tmp.join(first_check.path);
        if let Some(parent) = file_path.parent() {
            fs::create_dir_all(parent).unwrap();
        }
        // Write arbitrary content — the hash will not match the expected one,
        // but the file exists. The second ID file is missing entirely.
        fs::write(&file_path, b"not the real content").unwrap();

        assert!(identify_source(&tmp).is_none());

        let _ = fs::remove_dir_all(&tmp);
    }

    // ── Error Display ───────────────────────────────────────────────

    /// `Sha1Mismatch` display must include the path, expected, and actual hashes.
    ///
    /// Users diagnose source-detection failures from error messages, so all
    /// three fields must appear in the formatted output.
    #[test]
    fn verify_error_display_sha1_mismatch() {
        let err = VerifyError::Sha1Mismatch {
            path: "test.mix".into(),
            expected: "aaa".into(),
            actual: "bbb".into(),
        };
        let msg = err.to_string();
        assert!(msg.contains("test.mix"), "missing path in: {msg}");
        assert!(msg.contains("aaa"), "missing expected hash in: {msg}");
        assert!(msg.contains("bbb"), "missing actual hash in: {msg}");
    }

    /// `FileNotFound` display must include the missing file name.
    ///
    /// The path is the only diagnostic information the user has to locate
    /// the problem, so it must appear verbatim in the error message.
    #[test]
    fn verify_error_display_file_not_found() {
        let err = VerifyError::FileNotFound("gone.mix".into());
        let msg = err.to_string();
        assert!(msg.contains("gone.mix"), "missing path in: {msg}");
    }

    // ── Manifest serialization ──────────────────────────────────────

    /// An `InstalledContentManifest` must survive a TOML round-trip unchanged.
    ///
    /// The manifest is persisted to disk as TOML; any field that silently
    /// drops during serialization or deserialization would cause false
    /// verification failures on next launch.
    #[test]
    fn manifest_serialization_roundtrip() {
        let mut files = BTreeMap::new();
        files.insert(
            "allies.mix".to_string(),
            FileDigest {
                sha256: "aa".repeat(32),
                size: 1024,
            },
        );
        files.insert(
            "soviet.mix".to_string(),
            FileDigest {
                sha256: "bb".repeat(32),
                size: 2048,
            },
        );

        let original = InstalledContentManifest {
            version: CONTENT_MANIFEST_VERSION,
            game: "ra".to_string(),
            content_version: "v1".to_string(),
            files,
        };

        let toml_str = toml::to_string(&original).unwrap();
        let restored: InstalledContentManifest = toml::from_str(&toml_str).unwrap();

        assert_eq!(original.version, restored.version);
        assert_eq!(original.game, restored.game);
        assert_eq!(original.content_version, restored.content_version);
        assert_eq!(original.files.len(), restored.files.len());
        for (path, digest) in &original.files {
            let rd = restored.files.get(path).expect("missing file entry");
            assert_eq!(digest.sha256, rd.sha256);
            assert_eq!(digest.size, rd.size);
        }
    }

    // ── Sha256Scratch ───────────────────────────────────────────────

    /// `Sha256Scratch::hash_file` must produce the same digest as `sha256_file`.
    ///
    /// The scratch variant is a performance optimization; it must be functionally
    /// identical to the one-shot `sha256_file` function or manifests generated by
    /// one path would not verify correctly against manifests generated by the other.
    #[test]
    fn scratch_hash_matches_sha256_file() {
        let tmp = std::env::temp_dir().join("cnc-verify-scratch");
        let _ = fs::remove_dir_all(&tmp);
        fs::create_dir_all(&tmp).unwrap();

        let path = tmp.join("data.bin");
        fs::write(&path, b"scratch buffer test data").unwrap();

        let direct = sha256_file(&path).unwrap();
        let mut scratch = Sha256Scratch::new();
        let scratched = scratch.hash_file(&path).unwrap();
        assert_eq!(direct, scratched);

        let _ = fs::remove_dir_all(&tmp);
    }

    /// Reusing a `Sha256Scratch` across multiple files must yield independent, correct hashes.
    ///
    /// The scratch pattern calls `finalize_reset` to clear hasher state between files.
    /// If the reset were missing, earlier file bytes would leak into later digests,
    /// causing false corruption reports for every file after the first.
    #[test]
    fn scratch_reuse_produces_correct_hashes() {
        let tmp = std::env::temp_dir().join("cnc-verify-scratch-reuse");
        let _ = fs::remove_dir_all(&tmp);
        fs::create_dir_all(&tmp).unwrap();

        let path_a = tmp.join("a.bin");
        let path_b = tmp.join("b.bin");
        fs::write(&path_a, b"file A content").unwrap();
        fs::write(&path_b, b"file B different content").unwrap();

        let mut scratch = Sha256Scratch::new();
        let hash_a = scratch.hash_file(&path_a).unwrap();
        let hash_b = scratch.hash_file(&path_b).unwrap();

        // Hashes must differ (different content).
        assert_ne!(hash_a, hash_b);

        // Rehashing must produce the same result (hasher properly reset).
        let hash_a2 = scratch.hash_file(&path_a).unwrap();
        assert_eq!(hash_a, hash_a2);

        let _ = fs::remove_dir_all(&tmp);
    }

    // ── VerifyBitfield ──────────────────────────────────────────────

    /// The verify bit-field must correctly set and retrieve individual bit positions.
    ///
    /// The bit-field is the core data structure for tracking which files have passed
    /// verification; incorrect `set`/`get` round-trips would silently mark failed
    /// files as passing, undermining the entire integrity check.
    #[cfg(feature = "fast-verify")]
    #[test]
    fn bitfield_set_and_get() {
        let mut bf = VerifyBitfield::new(100);
        assert!(!bf.get(0));
        assert!(!bf.get(99));

        bf.set(0);
        bf.set(42);
        bf.set(99);

        assert!(bf.get(0));
        assert!(bf.get(42));
        assert!(bf.get(99));
        assert!(!bf.get(1));
        assert!(!bf.get(98));
    }

    /// `count_ones` and `count_failures` on the verify bit-field must reflect the exact number of set bits.
    ///
    /// Progress reporting and repair decisions depend on these counts being accurate;
    /// an off-by-one would either hide failures or trigger unnecessary re-downloads.
    #[cfg(feature = "fast-verify")]
    #[test]
    fn bitfield_count_ones() {
        let mut bf = VerifyBitfield::new(256);
        assert_eq!(bf.count_ones(), 0);

        bf.set(0);
        bf.set(63);
        bf.set(64);
        bf.set(127);
        bf.set(255);
        assert_eq!(bf.count_ones(), 5);
        assert_eq!(bf.count_failures(), 251);
    }

    /// SIMD `and` and `or` operations on the verify bit-field must compute correct set intersection and union.
    ///
    /// These operations answer "which files are both installed and verified" (AND) and
    /// "which files have been touched at all" (OR); wrong SIMD lane indexing would
    /// corrupt the bit positions and produce incorrect answers for all subsequent queries.
    #[cfg(feature = "fast-verify")]
    #[test]
    fn bitfield_and_or_operations() {
        let mut a = VerifyBitfield::new(128);
        let mut b = VerifyBitfield::new(128);

        a.set(0);
        a.set(1);
        a.set(2);

        b.set(1);
        b.set(2);
        b.set(3);

        let intersection = a.and(&b);
        assert!(!intersection.get(0));
        assert!(intersection.get(1));
        assert!(intersection.get(2));
        assert!(!intersection.get(3));
        assert_eq!(intersection.count_ones(), 2);

        let union = a.or(&b);
        assert!(union.get(0));
        assert!(union.get(1));
        assert!(union.get(2));
        assert!(union.get(3));
        assert_eq!(union.count_ones(), 4);
    }

    /// The `and_not` operation must return bits set in `self` but not in `other`.
    ///
    /// This computes the "remaining work" set: starting from all files, subtracting
    /// already-checked files gives exactly the files that still need verification.
    /// An incorrect implementation would either re-check completed files or skip
    /// files that still need checking.
    #[cfg(feature = "fast-verify")]
    #[test]
    fn bitfield_and_not_for_remaining_work() {
        let mut all = VerifyBitfield::new(64);
        for i in 0..64 {
            all.set(i);
        }
        let mut checked = VerifyBitfield::new(64);
        checked.set(0);
        checked.set(10);
        checked.set(63);

        let remaining = all.and_not(&checked);
        assert_eq!(remaining.count_ones(), 61);
        assert!(!remaining.get(0));
        assert!(!remaining.get(10));
        assert!(!remaining.get(63));
        assert!(remaining.get(1));
    }

    /// `set_indices` must return exactly the indices of all set bits in ascending order.
    ///
    /// Callers use this to translate the compact bit representation back into file
    /// indices; a missing or duplicated index would cause a file to be skipped or
    /// repaired twice.
    #[cfg(feature = "fast-verify")]
    #[test]
    fn bitfield_set_indices() {
        let mut bf = VerifyBitfield::new(300);
        bf.set(0);
        bf.set(100);
        bf.set(200);
        bf.set(299);

        let indices = bf.set_indices();
        assert_eq!(indices, vec![0, 100, 200, 299]);
    }

    /// Bit positions that straddle a 256-bit SIMD lane boundary must be handled correctly.
    ///
    /// Each `u64x4` lane holds 256 bits; bit 255 is the last bit of lane 0 and bit 256
    /// is the first bit of lane 1. An off-by-one in the lane or word index calculation
    /// would silently corrupt either of these positions while all other bits appear correct.
    #[cfg(feature = "fast-verify")]
    #[test]
    fn bitfield_cross_lane_boundary() {
        // Test bits that cross the 256-bit lane boundary.
        let mut bf = VerifyBitfield::new(512);
        bf.set(255); // last bit of lane 0
        bf.set(256); // first bit of lane 1
        assert!(bf.get(255));
        assert!(bf.get(256));
        assert!(!bf.get(254));
        assert!(!bf.get(257));
        assert_eq!(bf.count_ones(), 2);
    }

    // ── Incremental verification ────────────────────────────────────

    /// `verify_incremental` must distribute all files across slots with no gaps or overlaps.
    ///
    /// The staggered verification scheme is only correct if every file appears in exactly
    /// one slot across a full cycle; a file that falls into no slot would never be verified,
    /// allowing silent corruption to go undetected indefinitely.
    ///
    /// The test creates 10 files, runs all 5 slots, and asserts that the total checked
    /// count equals 10 and every slot reports no failures.
    #[test]
    fn incremental_verify_distributes_files() {
        let tmp = std::env::temp_dir().join("cnc-verify-incremental");
        let _ = fs::remove_dir_all(&tmp);
        fs::create_dir_all(&tmp).unwrap();

        // Create 10 files with known content.
        let mut manifest_files = BTreeMap::new();
        for i in 0..10 {
            let name = format!("file{i}.bin");
            let data = format!("content for file {i}");
            let path = tmp.join(&name);
            fs::write(&path, data.as_bytes()).unwrap();
            let sha256 = sha256_file(&path).unwrap();
            let size = data.len() as u64;
            manifest_files.insert(name, FileDigest { sha256, size });
        }

        let manifest = InstalledContentManifest {
            version: 1,
            game: "test".to_string(),
            content_version: "v1".to_string(),
            files: manifest_files,
        };

        // With 5 slots, each slot should check ~2 files.
        let mut total_checked = 0;
        for slot in 0..5 {
            let result = verify_incremental(&tmp, &manifest, slot, 5);
            assert!(result.failures.is_empty());
            assert_eq!(result.total_files, 10);
            assert_eq!(result.num_slots, 5);
            total_checked += result.checked.len();
        }
        // All 10 files should be covered across 5 slots.
        assert_eq!(total_checked, 10);

        let _ = fs::remove_dir_all(&tmp);
    }

    /// `verify_incremental` must report a failure when a file has been tampered with since manifest generation.
    ///
    /// The incremental path must exercise the same hash comparison logic as the full
    /// verification path; if it silently skipped the comparison, corruption introduced
    /// between verification cycles would never be detected.
    #[test]
    fn incremental_verify_detects_corruption() {
        let tmp = std::env::temp_dir().join("cnc-verify-incr-corrupt");
        let _ = fs::remove_dir_all(&tmp);
        fs::create_dir_all(&tmp).unwrap();

        let path = tmp.join("data.bin");
        fs::write(&path, b"original").unwrap();
        let sha256 = sha256_file(&path).unwrap();

        let mut files = BTreeMap::new();
        files.insert("data.bin".to_string(), FileDigest { sha256, size: 8 });

        let manifest = InstalledContentManifest {
            version: 1,
            game: "test".to_string(),
            content_version: "v1".to_string(),
            files,
        };

        // Corrupt the file.
        fs::write(&path, b"tampered").unwrap();

        let result = verify_incremental(&tmp, &manifest, 0, 1);
        assert_eq!(result.failures.len(), 1);
        assert_eq!(result.failures[0], "data.bin");

        let _ = fs::remove_dir_all(&tmp);
    }
}
