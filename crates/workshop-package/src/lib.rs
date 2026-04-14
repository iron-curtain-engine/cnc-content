// SPDX-License-Identifier: MIT OR Apache-2.0

//! Workshop package format — ZIP archives with content-addressed integrity.
//!
//! A Workshop package (`.icpkg` for Iron Curtain, `.xpkg`/`.cpkg` for other
//! projects) is a ZIP archive containing:
//!
//! - `manifest.json` — package metadata, dependency declarations, and per-file
//!   SHA-256 checksums (first entry in the ZIP for fast remote metadata reads)
//! - `assets/` — content files (sprites, maps, audio, scripts)
//! - `README.md` (optional) — human-readable documentation
//! - `preview.png` (optional) — listing thumbnail (max 512×512)
//!
//! # Content addressing
//!
//! Every file in the archive is identified by its SHA-256 hash in the manifest.
//! The archive itself is identified by the [`BlobId`] (SHA-256 of the entire
//! ZIP). This enables content-addressed deduplication: two package versions
//! sharing the same sprite sheet share the same blob in the CAS store.
//!
//! # Design authority
//!
//! - D030 §Package Format — ZIP container, manifest at offset 0, per-file hashes
//! - D050 §Three-Layer Architecture — game-agnostic package format
//! - `dependency-resolution-design.md` §12 — package checksum integration

use std::io::{Cursor, Read, Write};

use sha2::{Digest, Sha256};
use workshop_core::BlobId;

// ── Path validation ──────────────────────────────────────────────────

/// Maximum asset path length to prevent resource exhaustion.
const MAX_ASSET_PATH_LEN: usize = 256;

/// Maximum manifest.json size (1 MB).
///
/// Real manifests are a few KB. 1 MB is generous for any realistic package
/// while preventing OOM from a crafted archive containing a multi-GB manifest.
const MAX_MANIFEST_SIZE: u64 = 1_048_576;

/// Windows reserved device names that must be rejected in asset paths.
///
/// Creating a file named `CON`, `NUL`, `PRN`, etc. on Windows causes hangs,
/// data loss, or unpredictable behavior. These names are reserved regardless
/// of extension (e.g. `CON.txt` is still reserved on Windows).
const WINDOWS_RESERVED_NAMES: &[&str] = &[
    "CON", "PRN", "AUX", "NUL", "COM0", "COM1", "COM2", "COM3", "COM4", "COM5", "COM6", "COM7",
    "COM8", "COM9", "LPT0", "LPT1", "LPT2", "LPT3", "LPT4", "LPT5", "LPT6", "LPT7", "LPT8", "LPT9",
];

/// Validates that an asset path is safe to embed in or read from a package.
///
/// Rejects paths that could enable path traversal when the package is
/// extracted by a downstream consumer. Called during both [`build_package`]
/// (defense-in-depth on trusted input) and [`read_manifest`] (untrusted
/// input from archive).
///
/// ## Rejected patterns
///
/// - Empty paths
/// - Parent traversal (`..` components in any position)
/// - Absolute paths (leading `/`)
/// - Windows drive letters (`C:`, `D:`, etc.)
/// - Backslash separators (Windows path separators)
/// - Null bytes (C-string truncation attacks)
/// - Paths exceeding length limit
fn validate_asset_path(path: &str) -> Result<(), PackageError> {
    if path.is_empty() {
        return Err(PackageError::PathTraversal {
            path: path.to_string(),
            detail: "empty path".to_string(),
        });
    }

    if path.len() > MAX_ASSET_PATH_LEN {
        return Err(PackageError::PathTraversal {
            path: path.to_string(),
            detail: format!(
                "path length {} exceeds limit {MAX_ASSET_PATH_LEN}",
                path.len()
            ),
        });
    }

    // Reject null bytes (C-string truncation attacks).
    if path.contains('\0') {
        return Err(PackageError::PathTraversal {
            path: path.to_string(),
            detail: "contains null byte".to_string(),
        });
    }

    // Reject backslashes (Windows path separators in archive metadata).
    if path.contains('\\') {
        return Err(PackageError::PathTraversal {
            path: path.to_string(),
            detail: "contains backslash".to_string(),
        });
    }

    // Reject absolute paths (leading /).
    if path.starts_with('/') {
        return Err(PackageError::PathTraversal {
            path: path.to_string(),
            detail: "absolute path".to_string(),
        });
    }

    // Reject Windows drive letters (e.g. C:\path, D:file).
    let bytes = path.as_bytes();
    if let (Some(&first), Some(&b':')) = (bytes.first(), bytes.get(1)) {
        if first.is_ascii_alphabetic() {
            return Err(PackageError::PathTraversal {
                path: path.to_string(),
                detail: "Windows drive letter".to_string(),
            });
        }
    }

    // Reject parent traversal (..) in any path component.
    // Also reject Windows reserved device names in any component.
    for component in path.split('/') {
        if component == ".." {
            return Err(PackageError::PathTraversal {
                path: path.to_string(),
                detail: "parent traversal (..)".to_string(),
            });
        }

        // Check for Windows reserved device names. Strip extension first
        // because `CON.txt` is still reserved on Windows.
        let stem = component.split('.').next().unwrap_or(component);
        if WINDOWS_RESERVED_NAMES
            .iter()
            .any(|&r| r.eq_ignore_ascii_case(stem))
        {
            return Err(PackageError::PathTraversal {
                path: path.to_string(),
                detail: format!("Windows reserved device name: {component}"),
            });
        }
    }

    Ok(())
}

// ── Package metadata ─────────────────────────────────────────────────

/// In-package manifest — the canonical metadata for a Workshop package.
///
/// This is serialized as `manifest.json` inside the ZIP archive as the
/// first entry, enabling fast metadata reads via byte-range requests
/// without downloading the entire archive.
///
/// # Note on format
///
/// The design doc specifies `manifest.yaml`. This implementation uses JSON
/// to avoid adding a YAML dependency. The format is an internal detail
/// that can be migrated without changing the public API.
#[derive(Debug, Clone, serde::Deserialize, serde::Serialize)]
pub struct PackageMeta {
    /// Package name (without publisher scope).
    pub name: String,
    /// Publisher scope.
    pub publisher: String,
    /// Exact semver version.
    pub version: String,
    /// SPDX license expression.
    pub license: String,
    /// Required engine version range (e.g. `"^0.3"`). `None` for engine-agnostic.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub engine_version: Option<String>,
    /// Human-readable description.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub description: Option<String>,
    /// Content files with per-file integrity checksums.
    #[serde(default)]
    pub files: Vec<FileEntry>,
    /// Dependencies on other Workshop packages.
    #[serde(default)]
    pub dependencies: Vec<MetaDependency>,
}

/// A content file entry with its SHA-256 checksum.
///
/// The checksum enables per-file integrity verification and content-addressed
/// deduplication in the blob store. Two files with identical content produce
/// the same `sha256` regardless of their path or package.
#[derive(Debug, Clone, serde::Deserialize, serde::Serialize)]
pub struct FileEntry {
    /// Relative path within `assets/` (e.g. `"sprites/infantry.shp"`).
    pub path: String,
    /// Lowercase hex-encoded SHA-256 of the file content.
    pub sha256: String,
    /// File size in bytes.
    pub size: u64,
}

/// A dependency declaration within the in-package manifest.
#[derive(Debug, Clone, serde::Deserialize, serde::Serialize)]
pub struct MetaDependency {
    /// Full `publisher/name` identity.
    pub package: String,
    /// Semver version requirement.
    pub version_req: String,
    /// Whether this dependency is optional.
    #[serde(default)]
    pub optional: bool,
}

// ── Package specification (input to build) ───────────────────────────

/// Input specification for building a package archive.
///
/// Contains the identity fields but not file checksums — those are
/// computed during [`build_package`].
pub struct PackageSpec {
    /// Package name.
    pub name: String,
    /// Publisher scope.
    pub publisher: String,
    /// Semver version.
    pub version: String,
    /// SPDX license.
    pub license: String,
    /// Optional description.
    pub description: Option<String>,
    /// Optional engine version requirement.
    pub engine_version: Option<String>,
    /// Dependencies.
    pub dependencies: Vec<MetaDependency>,
}

/// The output of building a package archive.
pub struct BuiltPackage {
    /// The raw ZIP archive bytes.
    pub archive: Vec<u8>,
    /// SHA-256 content hash of the archive (for registry `cksum` field).
    pub blob_id: BlobId,
    /// The computed manifest (includes per-file checksums).
    pub manifest: PackageMeta,
}

// ── Build ────────────────────────────────────────────────────────────

/// Build a Workshop package archive from a spec and file contents.
///
/// Computes per-file SHA-256 checksums, writes the manifest as the first
/// ZIP entry (`manifest.json`), then writes each content file under
/// `assets/`. Returns the archive bytes, its `BlobId`, and the computed
/// manifest.
///
/// # File ordering
///
/// `manifest.json` is always the first entry in the ZIP so that remote
/// metadata reads can extract it with a small byte-range request.
pub fn build_package(
    spec: &PackageSpec,
    files: &[(&str, &[u8])],
) -> Result<BuiltPackage, PackageError> {
    // Validate all file paths before embedding in the archive.
    // Defense-in-depth: callers should provide clean paths, but we verify
    // to prevent malicious metadata from propagating downstream.
    for &(path, _) in files {
        validate_asset_path(path)?;
    }

    // Compute per-file checksums.
    let file_entries: Vec<FileEntry> = files
        .iter()
        .map(|(path, data)| {
            let mut hasher = Sha256::new();
            hasher.update(data);
            let hash = hasher.finalize();
            let hex = hash.iter().fold(String::with_capacity(64), |mut s, b| {
                use std::fmt::Write;
                let _ = write!(s, "{b:02x}");
                s
            });
            FileEntry {
                path: (*path).to_string(),
                sha256: hex,
                size: data.len() as u64,
            }
        })
        .collect();

    let manifest = PackageMeta {
        name: spec.name.clone(),
        publisher: spec.publisher.clone(),
        version: spec.version.clone(),
        license: spec.license.clone(),
        engine_version: spec.engine_version.clone(),
        description: spec.description.clone(),
        files: file_entries,
        dependencies: spec.dependencies.clone(),
    };

    // Serialize manifest to JSON.
    let manifest_json =
        serde_json::to_string_pretty(&manifest).map_err(|err| PackageError::ManifestSerialize {
            message: err.to_string(),
        })?;

    // Build ZIP archive with manifest as first entry.
    let buf = Vec::new();
    let cursor = Cursor::new(buf);
    let mut zip = zip::ZipWriter::new(cursor);
    let options = zip::write::SimpleFileOptions::default()
        .compression_method(zip::CompressionMethod::Deflated);

    // First entry: manifest.json.
    zip.start_file("manifest.json", options)
        .map_err(|err| PackageError::ZipWrite {
            message: err.to_string(),
        })?;
    zip.write_all(manifest_json.as_bytes())
        .map_err(|err| PackageError::ZipWrite {
            message: err.to_string(),
        })?;

    // Content files under assets/.
    for (path, data) in files {
        zip.start_file(format!("assets/{path}"), options)
            .map_err(|err| PackageError::ZipWrite {
                message: err.to_string(),
            })?;
        zip.write_all(data).map_err(|err| PackageError::ZipWrite {
            message: err.to_string(),
        })?;
    }

    let cursor = zip.finish().map_err(|err| PackageError::ZipWrite {
        message: err.to_string(),
    })?;
    let archive = cursor.into_inner();
    let blob_id = BlobId::from_data(&archive);

    Ok(BuiltPackage {
        archive,
        blob_id,
        manifest,
    })
}

// ── Read ─────────────────────────────────────────────────────────────

/// Read the manifest from a package archive.
///
/// Opens the ZIP and reads the `manifest.json` entry. Returns the parsed
/// [`PackageMeta`] without extracting any content files.
pub fn read_manifest(archive: &[u8]) -> Result<PackageMeta, PackageError> {
    let cursor = Cursor::new(archive);
    let mut zip = zip::ZipArchive::new(cursor).map_err(|err| PackageError::ZipRead {
        message: err.to_string(),
    })?;

    let mut manifest_file =
        zip.by_name("manifest.json")
            .map_err(|err| PackageError::ManifestMissing {
                message: err.to_string(),
            })?;

    // Guard against OOM from a crafted archive with a multi-GB manifest.json.
    // Real manifests are a few KB. The zip crate reports decompressed size.
    if manifest_file.size() > MAX_MANIFEST_SIZE {
        return Err(PackageError::ManifestParse {
            message: format!(
                "manifest.json is {} bytes (max {MAX_MANIFEST_SIZE})",
                manifest_file.size()
            ),
        });
    }

    let mut content = String::new();
    manifest_file
        .read_to_string(&mut content)
        .map_err(|err| PackageError::ManifestParse {
            message: err.to_string(),
        })?;

    // Double-check actual size after decompression — the declared size
    // in the ZIP central directory could lie.
    if content.len() as u64 > MAX_MANIFEST_SIZE {
        return Err(PackageError::ManifestParse {
            message: format!(
                "manifest.json decompressed to {} bytes (max {MAX_MANIFEST_SIZE})",
                content.len()
            ),
        });
    }

    let manifest: PackageMeta =
        serde_json::from_str(&content).map_err(|err| PackageError::ManifestParse {
            message: err.to_string(),
        })?;

    // Validate all declared file paths — these are untrusted input from the
    // archive and could contain traversal sequences that would be exploited
    // when a downstream consumer extracts files by manifest path.
    for entry in &manifest.files {
        validate_asset_path(&entry.path)?;
    }

    Ok(manifest)
}

// ── Validation ───────────────────────────────────────────────────────

/// Result of package validation.
#[derive(Debug)]
pub struct ValidationResult {
    /// Number of files checked.
    pub files_checked: usize,
    /// Files whose checksum did not match the manifest.
    pub mismatches: Vec<ValidationMismatch>,
}

/// A file whose actual checksum did not match the manifest declaration.
#[derive(Debug)]
pub struct ValidationMismatch {
    /// File path from the manifest.
    pub path: String,
    /// Expected SHA-256 hex from the manifest.
    pub expected: String,
    /// Actual SHA-256 hex of the file content.
    pub actual: String,
}

impl ValidationResult {
    /// Returns `true` if all files passed integrity checks.
    pub fn is_valid(&self) -> bool {
        self.mismatches.is_empty()
    }
}

/// Validate a package archive against its embedded manifest checksums.
///
/// Reads the manifest, then for each declared file, reads the content from
/// the ZIP and verifies the SHA-256 hash matches. Returns a
/// [`ValidationResult`] with any mismatches.
pub fn validate_package(archive: &[u8]) -> Result<ValidationResult, PackageError> {
    let manifest = read_manifest(archive)?;

    let cursor = Cursor::new(archive);
    let mut zip = zip::ZipArchive::new(cursor).map_err(|err| PackageError::ZipRead {
        message: err.to_string(),
    })?;

    let mut mismatches = Vec::new();

    for file_entry in &manifest.files {
        let zip_path = format!("assets/{}", file_entry.path);
        let mut zip_file = zip
            .by_name(&zip_path)
            .map_err(|err| PackageError::FileMissing {
                path: file_entry.path.clone(),
                message: err.to_string(),
            })?;

        // Hash the actual content.
        let mut hasher = Sha256::new();
        let mut buf = [0u8; 8192];
        loop {
            let n = zip_file
                .read(&mut buf)
                .map_err(|err| PackageError::ZipRead {
                    message: err.to_string(),
                })?;
            if n == 0 {
                break;
            }
            let chunk = buf.get(..n).unwrap_or(&buf);
            hasher.update(chunk);
        }
        let hash = hasher.finalize();
        let actual_hex = hash.iter().fold(String::with_capacity(64), |mut s, b| {
            use std::fmt::Write;
            let _ = write!(s, "{b:02x}");
            s
        });

        if actual_hex != file_entry.sha256 {
            mismatches.push(ValidationMismatch {
                path: file_entry.path.clone(),
                expected: file_entry.sha256.clone(),
                actual: actual_hex,
            });
        }
    }

    Ok(ValidationResult {
        files_checked: manifest.files.len(),
        mismatches,
    })
}

// ── Error types ──────────────────────────────────────────────────────

/// Errors from package operations.
#[derive(Debug, thiserror::Error)]
pub enum PackageError {
    /// Failed to write a ZIP archive entry.
    #[error("ZIP write error: {message}")]
    ZipWrite { message: String },

    /// Failed to read a ZIP archive.
    #[error("ZIP read error: {message}")]
    ZipRead { message: String },

    /// The `manifest.json` entry is missing from the archive.
    #[error("manifest.json missing from package: {message}")]
    ManifestMissing { message: String },

    /// Failed to parse the manifest JSON.
    #[error("manifest parse error: {message}")]
    ManifestParse { message: String },

    /// Failed to serialize the manifest to JSON.
    #[error("manifest serialization error: {message}")]
    ManifestSerialize { message: String },

    /// A file declared in the manifest is missing from the archive.
    #[error("file `{path}` missing from archive: {message}")]
    FileMissing { path: String, message: String },

    /// An asset path contains traversal sequences or is otherwise unsafe.
    ///
    /// Detected during [`build_package`] (defense-in-depth) or [`read_manifest`]
    /// (untrusted archive input). Prevents Zip Slip (CVE-2018-1002200) and
    /// related path-based attacks when the package is extracted downstream.
    #[error("path traversal in asset path `{path}`: {detail}")]
    PathTraversal { path: String, detail: String },
}

// ── Tests ────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;

    /// Helper: create a minimal PackageSpec for testing.
    fn test_spec() -> PackageSpec {
        PackageSpec {
            name: "test-mod".to_string(),
            publisher: "alice".to_string(),
            version: "1.0.0".to_string(),
            license: "MIT".to_string(),
            description: Some("A test package".to_string()),
            engine_version: None,
            dependencies: vec![],
        }
    }

    // ── Build and read round-trip ────────────────────────────────────

    /// A package built and then read back produces the same manifest.
    ///
    /// This is the fundamental correctness property: the builder and reader
    /// are inverse operations for the manifest data.
    #[test]
    fn build_and_read_round_trip() {
        let spec = test_spec();
        let files = vec![
            ("sprites/tank.shp", b"tank sprite data" as &[u8]),
            ("maps/desert.map", b"map data" as &[u8]),
        ];

        let built = build_package(&spec, &files).unwrap();
        let manifest = read_manifest(&built.archive).unwrap();

        assert_eq!(manifest.name, "test-mod");
        assert_eq!(manifest.publisher, "alice");
        assert_eq!(manifest.version, "1.0.0");
        assert_eq!(manifest.license, "MIT");
        assert_eq!(manifest.files.len(), 2);
    }

    /// The manifest includes correct per-file SHA-256 checksums.
    ///
    /// Checksums enable content-addressed dedup in the blob store and
    /// per-file integrity verification after download.
    #[test]
    fn manifest_includes_file_hashes() {
        let data = b"deterministic content";
        let spec = test_spec();
        let files = vec![("test.txt", data as &[u8])];

        let built = build_package(&spec, &files).unwrap();

        assert_eq!(built.manifest.files.len(), 1);
        assert_eq!(built.manifest.files[0].path, "test.txt");
        assert_eq!(built.manifest.files[0].size, data.len() as u64);

        // Verify the hash is correct by computing it independently.
        let mut hasher = Sha256::new();
        hasher.update(data);
        let expected = hasher.finalize();
        let expected_hex = expected.iter().fold(String::with_capacity(64), |mut s, b| {
            use std::fmt::Write;
            let _ = write!(s, "{b:02x}");
            s
        });
        assert_eq!(built.manifest.files[0].sha256, expected_hex);
    }

    // ── Validation ───────────────────────────────────────────────────

    /// A cleanly built package passes validation.
    #[test]
    fn validate_clean_package() {
        let spec = test_spec();
        let files = vec![
            ("sprites/tank.shp", b"tank data" as &[u8]),
            ("audio/fire.aud", b"audio data" as &[u8]),
        ];

        let built = build_package(&spec, &files).unwrap();
        let result = validate_package(&built.archive).unwrap();

        assert!(result.is_valid());
        assert_eq!(result.files_checked, 2);
    }

    // ── BlobId determinism ───────────────────────────────────────────

    /// Building the same package twice produces the same BlobId.
    ///
    /// Deterministic builds are critical for content-addressed storage:
    /// the same input must always produce the same hash so that the
    /// registry checksum matches what users compute locally.
    #[test]
    fn blob_id_deterministic() {
        let spec = test_spec();
        let files = vec![("test.txt", b"same content" as &[u8])];

        let first = build_package(&spec, &files).unwrap();
        let second = build_package(&spec, &files).unwrap();

        assert_eq!(first.blob_id, second.blob_id);
        assert_eq!(first.archive, second.archive);
    }

    // ── Empty package ────────────────────────────────────────────────

    /// A package with no content files is valid (metadata-only package).
    #[test]
    fn empty_package_is_valid() {
        let spec = test_spec();
        let files: Vec<(&str, &[u8])> = vec![];

        let built = build_package(&spec, &files).unwrap();
        let manifest = read_manifest(&built.archive).unwrap();

        assert!(manifest.files.is_empty());
        let result = validate_package(&built.archive).unwrap();
        assert!(result.is_valid());
        assert_eq!(result.files_checked, 0);
    }

    // ── Dependencies ─────────────────────────────────────────────────

    /// Dependencies in the spec are preserved in the manifest.
    #[test]
    fn dependencies_in_manifest() {
        let spec = PackageSpec {
            name: "my-mod".to_string(),
            publisher: "alice".to_string(),
            version: "1.0.0".to_string(),
            license: "MIT".to_string(),
            description: None,
            engine_version: Some("^0.3".to_string()),
            dependencies: vec![
                MetaDependency {
                    package: "bob/sprites".to_string(),
                    version_req: "^1.0".to_string(),
                    optional: false,
                },
                MetaDependency {
                    package: "carol/effects".to_string(),
                    version_req: "^2.0".to_string(),
                    optional: true,
                },
            ],
        };

        let built = build_package(&spec, &[]).unwrap();
        let manifest = read_manifest(&built.archive).unwrap();

        assert_eq!(manifest.dependencies.len(), 2);
        assert_eq!(manifest.dependencies[0].package, "bob/sprites");
        assert!(!manifest.dependencies[0].optional);
        assert_eq!(manifest.dependencies[1].package, "carol/effects");
        assert!(manifest.dependencies[1].optional);
        assert_eq!(manifest.engine_version.as_deref(), Some("^0.3"));
    }

    // ── Error display ────────────────────────────────────────────────

    /// ManifestMissing error includes context about the missing file.
    #[test]
    fn error_display_manifest_missing() {
        let err = PackageError::ManifestMissing {
            message: "not found".to_string(),
        };
        let msg = err.to_string();
        assert!(msg.contains("manifest.json"), "{msg}");
        assert!(msg.contains("not found"), "{msg}");
    }

    /// FileMissing error includes the path and detail.
    #[test]
    fn error_display_file_missing() {
        let err = PackageError::FileMissing {
            path: "sprites/tank.shp".to_string(),
            message: "no such entry".to_string(),
        };
        let msg = err.to_string();
        assert!(msg.contains("sprites/tank.shp"), "{msg}");
    }

    // ── Security: path traversal (CVE-2018-1002200) ─────────────────

    /// build_package rejects file paths containing parent traversal.
    ///
    /// A malicious caller supplying `../../etc/passwd` as a file path would
    /// create a ZIP entry that, when extracted by a downstream consumer,
    /// writes outside the extraction directory (Zip Slip — CVE-2018-1002200).
    /// The path must be rejected before the ZIP is built.
    #[test]
    fn build_rejects_parent_traversal() {
        let spec = test_spec();
        let files = vec![("../../etc/passwd", b"pwned" as &[u8])];
        let result = build_package(&spec, &files);
        assert!(matches!(result, Err(PackageError::PathTraversal { .. })));
    }

    /// build_package rejects absolute file paths.
    ///
    /// An absolute path (`/etc/passwd`) would bypass the extraction directory
    /// entirely, writing to the root filesystem on extraction.
    #[test]
    fn build_rejects_absolute_path() {
        let spec = test_spec();
        let files = vec![("/etc/passwd", b"pwned" as &[u8])];
        let result = build_package(&spec, &files);
        assert!(matches!(result, Err(PackageError::PathTraversal { .. })));
    }

    /// build_package rejects backslash path separators.
    ///
    /// Backslashes are Windows path separators. A path containing `..\\..\\`
    /// would be a traversal attack on Windows. Even on Unix, backslashes in
    /// archive paths cause cross-platform extraction inconsistencies.
    #[test]
    fn build_rejects_backslash_path() {
        let spec = test_spec();
        let files = vec![("sprites\\tank.shp", b"data" as &[u8])];
        let result = build_package(&spec, &files);
        assert!(matches!(result, Err(PackageError::PathTraversal { .. })));
    }

    /// build_package rejects paths with null bytes.
    ///
    /// Null bytes can truncate paths in C-based extraction tools, causing
    /// `malicious.exe\0.txt` to be written as `malicious.exe` — a classic
    /// file extension bypass attack.
    #[test]
    fn build_rejects_null_byte_path() {
        let spec = test_spec();
        let files = vec![("sprites/tank\0.shp", b"data" as &[u8])];
        let result = build_package(&spec, &files);
        assert!(matches!(result, Err(PackageError::PathTraversal { .. })));
    }

    /// build_package rejects empty file paths.
    ///
    /// An empty path is semantically invalid — it cannot name a file — and
    /// would cause unpredictable behavior during extraction.
    #[test]
    fn build_rejects_empty_path() {
        let spec = test_spec();
        let files = vec![("", b"data" as &[u8])];
        let result = build_package(&spec, &files);
        assert!(matches!(result, Err(PackageError::PathTraversal { .. })));
    }

    /// build_package rejects Windows drive letter paths.
    ///
    /// A path like `C:secret.txt` could write to the C: drive root on
    /// Windows, bypassing the extraction directory.
    #[test]
    fn build_rejects_windows_drive_letter() {
        let spec = test_spec();
        let files = vec![("C:secret.txt", b"pwned" as &[u8])];
        let result = build_package(&spec, &files);
        assert!(matches!(result, Err(PackageError::PathTraversal { .. })));
    }

    /// build_package rejects embedded parent traversal.
    ///
    /// A path like `sprites/../../etc/passwd` normalises to a traversal
    /// even though the first component is innocent. Each path component
    /// must be checked individually.
    #[test]
    fn build_rejects_embedded_traversal() {
        let spec = test_spec();
        let files = vec![("sprites/../../etc/passwd", b"pwned" as &[u8])];
        let result = build_package(&spec, &files);
        assert!(matches!(result, Err(PackageError::PathTraversal { .. })));
    }

    /// build_package accepts valid nested asset paths.
    ///
    /// Security checks must not block legitimate paths. Nested subdirectories
    /// with standard forward-slash separators are valid.
    #[test]
    fn build_accepts_valid_nested_paths() {
        let spec = test_spec();
        let files = vec![
            ("sprites/infantry.shp", b"sprite" as &[u8]),
            ("maps/campaign/soviet/mission01.map", b"map" as &[u8]),
        ];
        let result = build_package(&spec, &files);
        assert!(result.is_ok(), "valid paths should be accepted");
    }

    /// read_manifest rejects manifests containing traversal paths.
    ///
    /// A malicious package could encode `../../etc/passwd` as a file path
    /// in the manifest. When a downstream tool extracts files by manifest
    /// path, this would cause Zip Slip (CVE-2018-1002200). The reader must
    /// reject the manifest at parse time before returning it to callers.
    #[test]
    fn read_manifest_rejects_traversal_path() {
        // Build a ZIP manually with a manifest containing a malicious path,
        // bypassing build_package()'s own validation.
        let manifest = PackageMeta {
            name: "evil".to_string(),
            publisher: "mallory".to_string(),
            version: "1.0.0".to_string(),
            license: "MIT".to_string(),
            engine_version: None,
            description: None,
            files: vec![FileEntry {
                path: "../../etc/passwd".to_string(),
                sha256: "deadbeef".to_string(),
                size: 5,
            }],
            dependencies: vec![],
        };
        let manifest_json = serde_json::to_string_pretty(&manifest).unwrap();
        let mut buf = Cursor::new(Vec::new());
        {
            let mut zip = zip::ZipWriter::new(&mut buf);
            let options = zip::write::SimpleFileOptions::default();
            zip.start_file("manifest.json", options).unwrap();
            zip.write_all(manifest_json.as_bytes()).unwrap();
            zip.finish().unwrap();
        }
        let archive = buf.into_inner();

        let result = read_manifest(&archive);
        assert!(
            matches!(result, Err(PackageError::PathTraversal { .. })),
            "should reject manifest with traversal path: {result:?}"
        );
    }

    /// read_manifest rejects manifests with backslash paths.
    ///
    /// Backslashes in manifest paths would cause platform-dependent
    /// extraction behavior — on Windows they're path separators that
    /// could enable traversal via `..\\..\\`.
    #[test]
    fn read_manifest_rejects_backslash_path() {
        let manifest = PackageMeta {
            name: "evil".to_string(),
            publisher: "mallory".to_string(),
            version: "1.0.0".to_string(),
            license: "MIT".to_string(),
            engine_version: None,
            description: None,
            files: vec![FileEntry {
                path: "sprites\\tank.shp".to_string(),
                sha256: "deadbeef".to_string(),
                size: 5,
            }],
            dependencies: vec![],
        };
        let manifest_json = serde_json::to_string_pretty(&manifest).unwrap();
        let mut buf = Cursor::new(Vec::new());
        {
            let mut zip = zip::ZipWriter::new(&mut buf);
            let options = zip::write::SimpleFileOptions::default();
            zip.start_file("manifest.json", options).unwrap();
            zip.write_all(manifest_json.as_bytes()).unwrap();
            zip.finish().unwrap();
        }
        let archive = buf.into_inner();

        let result = read_manifest(&archive);
        assert!(
            matches!(result, Err(PackageError::PathTraversal { .. })),
            "should reject manifest with backslash path: {result:?}"
        );
    }

    /// PathTraversal error includes the offending path and detail.
    #[test]
    fn error_display_path_traversal() {
        let err = PackageError::PathTraversal {
            path: "../../etc/passwd".to_string(),
            detail: "parent traversal (..)".to_string(),
        };
        let msg = err.to_string();
        assert!(msg.contains("../../etc/passwd"), "{msg}");
        assert!(msg.contains("parent traversal"), "{msg}");
    }

    // ── Security: Windows reserved device names ─────────────────────

    /// build_package rejects Windows reserved device names in file paths.
    ///
    /// Creating a file named `CON`, `NUL`, `PRN`, etc. on Windows causes
    /// hangs or data loss. These names must be rejected even when embedded
    /// in a subdirectory path (e.g. `sprites/CON.shp`).
    #[test]
    fn build_rejects_windows_device_name_con() {
        let spec = test_spec();
        let files = vec![("CON", b"data" as &[u8])];
        let result = build_package(&spec, &files);
        assert!(matches!(result, Err(PackageError::PathTraversal { .. })));
    }

    /// Device names are case-insensitive on Windows.
    #[test]
    fn build_rejects_windows_device_name_case_insensitive() {
        let spec = test_spec();
        let files = vec![("sprites/nul.shp", b"data" as &[u8])];
        let result = build_package(&spec, &files);
        assert!(matches!(result, Err(PackageError::PathTraversal { .. })));
    }

    /// COM and LPT ports are also reserved.
    #[test]
    fn build_rejects_windows_device_name_com1() {
        let spec = test_spec();
        let files = vec![("data/COM1.txt", b"data" as &[u8])];
        let result = build_package(&spec, &files);
        assert!(matches!(result, Err(PackageError::PathTraversal { .. })));
    }

    /// Non-reserved names that start with reserved prefixes are accepted.
    ///
    /// `CONSOLE.txt` is NOT a reserved name — only the exact stem `CON`
    /// (before any extension) is reserved.
    #[test]
    fn build_accepts_non_reserved_prefix() {
        let spec = test_spec();
        let files = vec![("CONSOLE/readme.txt", b"data" as &[u8])];
        let result = build_package(&spec, &files);
        assert!(result.is_ok(), "CONSOLE is not reserved, only CON is");
    }

    // ── Security: manifest.json size limit ──────────────────────────

    /// Manifest size limit constant is reasonable.
    ///
    /// Must be large enough for any realistic manifest while small enough
    /// to prevent OOM from a crafted archive.
    #[test]
    fn manifest_size_limit_is_sane() {
        const {
            assert!(
                MAX_MANIFEST_SIZE >= 100_000,
                "limit must handle realistic manifests"
            );
            assert!(MAX_MANIFEST_SIZE <= 10_000_000, "limit should prevent OOM");
        }
    }
}
