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
                files.insert(test_file.to_string(), FileDigest { sha256, size });
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

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::BTreeMap;

    // ── hex_encode ───────────────────────────────────────────────────

    #[test]
    fn hex_encode_empty() {
        assert_eq!(hex_encode(&[]), "");
    }

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

    #[test]
    fn hex_encode_is_lowercase() {
        let result = hex_encode(&[0xAB, 0xCD]);
        assert_eq!(result, "abcd");
        assert!(result.chars().all(|c| !c.is_ascii_uppercase()));
    }

    // ── sha1_file ────────────────────────────────────────────────────

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

    #[test]
    fn sha1_file_missing_returns_error() {
        let result = sha1_file(std::path::Path::new("/nonexistent/file.bin"), None);
        assert!(result.is_err());
    }

    // ── sha256_file ──────────────────────────────────────────────────

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

    #[test]
    fn identify_source_returns_none_for_empty_dir() {
        let tmp = std::env::temp_dir().join("cnc-verify-identify-empty");
        let _ = fs::remove_dir_all(&tmp);
        fs::create_dir_all(&tmp).unwrap();

        assert!(identify_source(&tmp).is_none());

        let _ = fs::remove_dir_all(&tmp);
    }
}
