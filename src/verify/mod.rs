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
            // `n` is bounded by `self.buffer.len()` since `file.read` was
            // given a `&mut self.buffer` slice — the `.get()` guards against
            // any future refactoring that changes the buffer reference.
            if let Some(slice) = self.buffer.get(..n) {
                self.hasher.update(slice);
            }
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
