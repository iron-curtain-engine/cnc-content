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
        let all_match = source.id_files.iter().all(|check| {
            verify_id_file(source_root, check).unwrap_or(false)
        });
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
                hasher.update(&buf[..n]);
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
        hasher.update(&buf[..n]);
    }

    Ok(hex_encode(hasher.finalize().as_slice()))
}

/// Encodes a byte slice as lowercase hex.
fn hex_encode(bytes: &[u8]) -> String {
    let mut s = String::with_capacity(bytes.len() * 2);
    for b in bytes {
        use std::fmt::Write;
        let _ = write!(s, "{b:02x}");
    }
    s
}

/// Verifies all installed content files against a manifest.
///
/// Returns a list of files that are missing or have mismatched hashes.
pub fn verify_installed_content(
    content_root: &Path,
    manifest: &InstalledContentManifest,
) -> Vec<String> {
    let mut failures = Vec::new();

    for (rel_path, expected) in &manifest.files {
        let full_path = content_root.join(rel_path);
        match sha256_file(&full_path) {
            Ok(actual) if actual == expected.sha256 => {}
            Ok(_actual) => failures.push(rel_path.clone()),
            Err(_) => failures.push(rel_path.clone()),
        }
    }

    failures
}

/// Generates an installed content manifest by hashing all files under the
/// content root.
pub fn generate_manifest(
    content_root: &Path,
    game: &str,
    content_version: &str,
    packages: &[PackageId],
) -> Result<InstalledContentManifest, io::Error> {
    let mut files = BTreeMap::new();

    // Walk all test files for the specified packages to find what we installed.
    for pkg_id in packages {
        let pkg = crate::package(*pkg_id);
        for &test_file in pkg.test_files {
            let full = content_root.join(test_file);
            if full.exists() {
                let sha256 = sha256_file(&full)?;
                let size = fs::metadata(&full)?.len();
                files.insert(
                    test_file.to_string(),
                    FileDigest { sha256, size },
                );
            }
        }
    }

    Ok(InstalledContentManifest {
        version: CONTENT_MANIFEST_VERSION,
        game: game.to_string(),
        content_version: content_version.to_string(),
        files,
    })
}
