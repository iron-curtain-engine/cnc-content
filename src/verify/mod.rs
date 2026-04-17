// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (c) 2025–present Iron Curtain contributors

//! Content verification — SHA-1 source identification and BLAKE3 installed checks.
//!
//! Two distinct hash families serve different roles:
//!
//! - **SHA-1**: used for OpenRA-compatible source identification (IDFiles).
//!   We must match OpenRA's published hashes to correctly identify disc editions,
//!   Steam installs, etc.
//!
//! - **BLAKE3**: used for our own installed-content manifest (`content-manifest.toml`).
//!   After content is extracted into managed storage, we hash every file with
//!   BLAKE3 for integrity verification, P2P torrent generation, and repair scans.
//!   BLAKE3 is 3–10× faster than SHA-256 on the same hardware thanks to a Merkle
//!   tree internal structure (enabling multithreaded single-file hashing via rayon),
//!   only 7 rounds per compression (vs SHA-256's 64), and automatic SIMD acceleration
//!   (SSE2/AVX2/AVX-512/NEON) without any manual intrinsics.
//!
//! ## Performance features (`fast-verify`)
//!
//! When the `fast-verify` feature is enabled:
//!
//! - **Parallel hashing**: `generate_manifest` and `verify_installed_content` use
//!   rayon to hash multiple files concurrently (~4x speedup on 4+ cores).
//! - **Scratch buffers**: `Blake3Scratch` pre-allocates a reusable read buffer,
//!   eliminating per-file allocation churn during batch verification.
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
///
/// Version history:
/// - **1**: SHA-256 digests (initial release).
/// - **2**: BLAKE3 digests — 3–10× faster hashing with SIMD auto-detection.
pub const CONTENT_MANIFEST_VERSION: u32 = 2;

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
    /// BLAKE3 hex digest for each installed file (sorted by path).
    pub files: BTreeMap<String, FileDigest>,
}

/// Per-file digest in the installed content manifest.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct FileDigest {
    /// BLAKE3 hex digest (lowercase, 64 characters).
    pub blake3: String,
    /// File size in bytes.
    pub size: u64,
}

/// Errors from verification operations.
#[derive(Debug, Error)]
pub enum VerifyError {
    #[error("I/O error: {source}")]
    Io {
        #[from]
        source: io::Error,
    },
    #[error("SHA-1 mismatch for {path}: expected {expected}, got {actual}")]
    Sha1Mismatch {
        path: String,
        expected: String,
        actual: String,
    },
    #[error("file not found: {path}")]
    FileNotFound { path: String },
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
            // Heap allocation here is intentional: prefix_length comes from
            // IdFileCheck config and is unbounded. Source identification runs
            // at most once per session, so this is not a hot path.
            let mut buf = vec![0u8; len as usize];
            file.read_exact(&mut buf)?;
            hasher.update(&buf);
        }
        None => {
            // 64 KiB read buffer — reduces syscall overhead by 8x vs 8 KiB.
            // Modern SSDs deliver 3+ GB/s; small buffers make the kernel
            // read() call the bottleneck rather than the hash computation.
            let mut buf = [0u8; 65536];
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

/// Computes the BLAKE3 hex digest of a file.
///
/// BLAKE3 automatically detects and uses the widest available SIMD
/// instruction set (SSE2 → AVX2 → AVX-512 on x86, NEON on ARM).
/// With only 7 rounds per compression (vs SHA-256's 64), this is
/// 3–10× faster than SHA-256 on modern hardware.
pub fn blake3_file(path: &Path) -> Result<String, io::Error> {
    let mut file = fs::File::open(path)?;
    let mut hasher = blake3::Hasher::new();
    // 64 KiB read buffer — reduces syscall overhead. BLAKE3's internal
    // compression operates on 1 KiB chunks, so 64 KiB feeds 64 chunks
    // per syscall, keeping the SIMD pipeline fully saturated.
    let mut buf = [0u8; 65536];

    loop {
        let n = file.read(&mut buf)?;
        if n == 0 {
            break;
        }
        hasher.update(buf.get(..n).unwrap_or(&[]));
    }

    Ok(hex_encode(hasher.finalize().as_bytes()))
}

/// Encodes a byte slice as lowercase hex.
///
/// Uses a direct lookup table instead of `fmt::Write` to avoid the
/// formatting machinery overhead. Each byte maps to two ASCII hex
/// digits via nibble extraction — no branching, no format parsing.
pub(crate) fn hex_encode(bytes: &[u8]) -> String {
    const HEX_DIGITS: &[u8; 16] = b"0123456789abcdef";
    let mut hex = Vec::with_capacity(bytes.len().saturating_mul(2));
    for &b in bytes {
        // b >> 4 is always 0..=15, b & 0x0f is always 0..=15, so .get()
        // always returns Some. The guards satisfy the safe-indexing rule.
        if let (Some(&hi), Some(&lo)) = (
            HEX_DIGITS.get((b >> 4) as usize),
            HEX_DIGITS.get((b & 0x0f) as usize),
        ) {
            hex.push(hi);
            hex.push(lo);
        }
    }
    // All bytes in hex[] are ASCII hex digits (0-9, a-f) — valid UTF-8.
    String::from_utf8(hex).unwrap_or_default()
}

// ── Scratch buffer pattern (IC distribution analysis §2.5) ──────────

/// Pre-allocated scratch space for BLAKE3 hashing.
///
/// Eliminates per-file allocation churn during batch verification by reusing
/// the read buffer. BLAKE3's `Hasher` is lightweight to create (1.7 KiB state)
/// and `reset()` is essentially free, but the 64 KiB read buffer is the real
/// allocation we avoid per-file. This is the same zero-allocation pattern used
/// by the sim's `TickScratch` — allocate once, reuse between files.
///
/// ## Usage
///
/// ```rust
/// use cnc_content::verify::Blake3Scratch;
///
/// let tmp = std::env::temp_dir().join("cnc-blake3-scratch-doctest");
/// let _ = std::fs::remove_dir_all(&tmp);
/// std::fs::create_dir_all(&tmp).unwrap();
/// std::fs::write(tmp.join("a.bin"), b"hello").unwrap();
/// std::fs::write(tmp.join("b.bin"), b"world").unwrap();
///
/// let mut scratch = Blake3Scratch::new();
/// let h1 = scratch.hash_file(&tmp.join("a.bin")).unwrap();
/// let h2 = scratch.hash_file(&tmp.join("b.bin")).unwrap();
/// // Different content produces different hashes.
/// assert_ne!(h1, h2);
/// // Same content produces same hash (scratch reuse is correct).
/// let h1b = scratch.hash_file(&tmp.join("a.bin")).unwrap();
/// assert_eq!(h1, h1b);
/// let _ = std::fs::remove_dir_all(&tmp);
/// ```
pub struct Blake3Scratch {
    buffer: Vec<u8>,
    hasher: blake3::Hasher,
}

impl Default for Blake3Scratch {
    fn default() -> Self {
        Self::new()
    }
}

impl Blake3Scratch {
    /// Creates a new scratch with a 64 KB read buffer.
    pub fn new() -> Self {
        Self {
            buffer: vec![0u8; 65536],
            hasher: blake3::Hasher::new(),
        }
    }

    /// Hashes a file using the pre-allocated buffer and hasher.
    ///
    /// The hasher is reset before each use — no allocation occurs.
    /// BLAKE3 automatically uses the widest SIMD instructions available.
    pub fn hash_file(&mut self, path: &Path) -> Result<String, io::Error> {
        self.hasher.reset();
        let mut file = fs::File::open(path)?;

        loop {
            let n = file.read(&mut self.buffer)?;
            if n == 0 {
                break;
            }
            // `n` is bounded by `self.buffer.len()` since `file.read` was
            // given a `&mut self.buffer` slice — the `.get()` guards against
            // any future refactoring that changes the buffer reference.
            if let Some(slice) = self.buffer.get(..n) {
                self.hasher.update(slice);
            }
        }

        Ok(hex_encode(self.hasher.finalize().as_bytes()))
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
    let mut scratch = Blake3Scratch::new();
    let mut failures = Vec::new();

    for (rel_path, expected) in &manifest.files {
        let full_path = content_root.join(rel_path);
        match scratch.hash_file(&full_path) {
            Ok(actual) if actual == expected.blake3 => {}
            // Clone only on failure — the common case (match) allocates nothing.
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
            let mut scratch = Blake3Scratch::new();
            match scratch.hash_file(&full_path) {
                Ok(actual) if actual == expected.blake3 => None,
                // Clone only on failure — the common case (match) allocates nothing.
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
        let pkg = crate::package(*pkg_id)
            .ok_or_else(|| io::Error::other(format!("no package definition for {pkg_id:?}")))?;
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
    let mut scratch = Blake3Scratch::new();
    let mut files = BTreeMap::new();

    for (test_file, full) in paths {
        let blake3 = scratch.hash_file(full)?;
        let size = fs::metadata(full)?.len();
        files.insert(test_file.to_string(), FileDigest { blake3, size });
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
            let mut scratch = Blake3Scratch::new();
            let blake3 = scratch.hash_file(full)?;
            let size = fs::metadata(full)?.len();
            Ok((test_file.to_string(), FileDigest { blake3, size }))
        })
        .collect();

    let mut files = BTreeMap::new();
    for result in results {
        let (path, digest) = result?;
        files.insert(path, digest);
    }
    Ok(files)
}

// ── Sub-modules ──────────────────────────────────────────────────────

/// SIMD verification bitfield (IC performance doc §2.5).
#[cfg(feature = "fast-verify")]
mod bitfield;
#[cfg(feature = "fast-verify")]
pub use bitfield::VerifyBitfield;

/// Incremental/staggered verification (IC distribution analysis §2.4).
mod incremental;
pub use incremental::{verify_incremental, IncrementalVerifyResult};

#[cfg(test)]
mod tests;
