// SPDX-License-Identifier: MIT OR Apache-2.0

//! Git-based package registry index for Workshop.
//!
//! This crate reads and writes the Workshop package index — a git repository
//! using crates.io-style file-per-package sharding. Each package file contains
//! one NDJSON (newline-delimited JSON) line per published version, enabling
//! efficient incremental sync via `git fetch`.
//!
//! The index is the Workshop's metadata layer: it maps `publisher/name@version`
//! to dependency declarations, checksums, and feature flags. Package content
//! (the actual mod assets) flows through `p2p-distribute` or HTTP mirrors —
//! the index only carries metadata (~200 bytes per version).
//!
//! # Index layout
//!
//! ```text
//! workshop-index/
//!   config.json           — registry metadata (download URL template, API URL)
//!   1/publisher/name      — 1-char package names
//!   2/publisher/name      — 2-char package names
//!   3/publisher/name      — 3-char package names
//!   ab/cd/publisher/name  — 4+ char names (first-2/next-2 sharding)
//! ```
//!
//! # Design authority
//!
//! Implements the registry index format from `dependency-resolution-design.md`
//! §4 (Registry Index Format) and Workshop index schema from D030.

use std::collections::HashMap;

use workshop_core::{BlobId, Dependency, PackageManifest, ResourceId, ResourceVersion};

// ── Index configuration ──────────────────────────────────────────────

/// Registry metadata from `config.json` at the index root.
///
/// Tells clients how to construct download URLs for resolved packages
/// and which federated registries are trusted as dependency sources.
#[derive(Debug, Clone, serde::Deserialize, serde::Serialize)]
pub struct RegistryConfig {
    /// Download URL template with `{publisher}`, `{name}`, `{version}` placeholders.
    pub dl: String,
    /// API endpoint URL for publish, yank, and search operations.
    pub api: String,
    /// Federated registries allowed as dependency sources.
    #[serde(default)]
    pub allowed_registries: Vec<String>,
}

// ── Registry entry (one version, one NDJSON line) ────────────────────

/// One published version of a package in the registry index.
///
/// Each non-blank line in the per-package NDJSON file deserializes to this
/// struct. Contains the full dependency and metadata snapshot needed for
/// dependency resolution without downloading the actual `.icpkg` archive.
///
/// # Immutability
///
/// Once appended to the index, a `RegistryEntry` is immutable — the version
/// can be yanked (hidden from new resolution) but never modified or deleted
/// (D030 Version Immutability).
#[derive(Debug, Clone, serde::Deserialize, serde::Serialize)]
pub struct RegistryEntry {
    /// Package name (without publisher prefix).
    pub name: String,
    /// Exact semver version string (e.g. `"1.2.3"`, `"0.1.0-beta.1"`).
    pub vers: String,
    /// Publisher scope (e.g. `"community-project"`).
    pub publisher: String,
    /// Dependencies declared by this version.
    #[serde(default)]
    pub deps: Vec<RegistryDep>,
    /// `sha256:` prefixed hex digest of the `.icpkg` archive.
    pub cksum: String,
    /// `sha256:` hex digest of the in-package manifest (D030 manifest confusion prevention).
    pub manifest_hash: String,
    /// Feature flag definitions: feature name → list of enabled optional deps.
    #[serde(default)]
    pub features: HashMap<String, Vec<String>>,
    /// Whether this version has been retracted by the publisher.
    #[serde(default)]
    pub yanked: bool,
    /// Sys-package link (prevents multiple packages linking the same native lib).
    #[serde(default)]
    pub links: Option<String>,
}

/// A dependency as recorded in a registry entry.
///
/// This is the serialized form stored in the NDJSON index. The resolver
/// parses the `req` field as a semver range at resolution time.
#[derive(Debug, Clone, serde::Deserialize, serde::Serialize)]
pub struct RegistryDep {
    /// Package name (without publisher prefix, e.g. `"base-palette"`).
    pub name: String,
    /// Publisher scope (e.g. `"ic-official"`).
    pub publisher: String,
    /// Semver version requirement string (e.g. `"^1.0"`, `">=2.0, <3.0"`).
    pub req: String,
    /// Dependency source type (usually `"workshop"`).
    #[serde(default = "default_source")]
    pub source: String,
    /// Whether this dependency is optional (requires feature activation).
    #[serde(default)]
    pub optional: bool,
    /// Whether to enable the dependency's default features.
    #[serde(default = "default_true")]
    pub default_features: bool,
    /// Additional features to enable on this dependency.
    #[serde(default)]
    pub features: Vec<String>,
}

fn default_source() -> String {
    "workshop".to_string()
}

fn default_true() -> bool {
    true
}

// ── Index path sharding ──────────────────────────────────────────────

/// Compute the relative path for a package in the sharded index.
///
/// Uses crates.io-style sharding based on the package name length:
/// - 1-char names: `1/{publisher}/{name}`
/// - 2-char names: `2/{publisher}/{name}`
/// - 3-char names: `3/{publisher}/{name}`
/// - 4+ char names: `{first2}/{next2}/{publisher}/{name}`
///
/// Publisher slugs and package names are lowercase ASCII, validated by
/// `ResourceId` construction, so all slicing is on single-byte chars.
pub fn index_shard_path(publisher: &str, name: &str) -> Result<String, RegistryError> {
    if name.is_empty() {
        return Err(RegistryError::InvalidPackageName {
            name: name.to_string(),
            reason: "package name cannot be empty".to_string(),
        });
    }
    if publisher.is_empty() {
        return Err(RegistryError::InvalidPackageName {
            name: publisher.to_string(),
            reason: "publisher cannot be empty".to_string(),
        });
    }
    let path = match name.len() {
        1 => format!("1/{publisher}/{name}"),
        2 => format!("2/{publisher}/{name}"),
        3 => format!("3/{publisher}/{name}"),
        _ => {
            // Safe: name is ASCII-only (ResourceId slug rules guarantee this),
            // so byte positions equal char positions. name.len() >= 4.
            let first2 = name.get(..2).unwrap_or(name);
            let next2 = name.get(2..4).unwrap_or(name.get(2..).unwrap_or(""));
            format!("{first2}/{next2}/{publisher}/{name}")
        }
    };
    Ok(path)
}

// ── NDJSON parsing ───────────────────────────────────────────────────

/// Parse a package index file (NDJSON: one JSON object per line).
///
/// Blank lines and lines starting with `#` are silently skipped.
/// Each non-blank line must be a valid JSON object deserializable as
/// a [`RegistryEntry`]. Returns entries in file order (oldest first for
/// append-only index files).
pub fn parse_index_file(content: &str) -> Result<Vec<RegistryEntry>, RegistryError> {
    let mut entries = Vec::new();
    for (line_idx, line) in content.lines().enumerate() {
        let trimmed = line.trim();
        if trimmed.is_empty() || trimmed.starts_with('#') {
            continue;
        }
        let entry: RegistryEntry =
            serde_json::from_str(trimmed).map_err(|err| RegistryError::ParseError {
                line: line_idx.saturating_add(1),
                message: err.to_string(),
            })?;
        entries.push(entry);
    }
    Ok(entries)
}

/// Serialize a registry entry to a single NDJSON line (no trailing newline).
pub fn serialize_entry(entry: &RegistryEntry) -> Result<String, RegistryError> {
    serde_json::to_string(entry).map_err(|err| RegistryError::SerializeError {
        message: err.to_string(),
    })
}

// ── Reader trait ─────────────────────────────────────────────────────

/// Read package metadata from a registry index.
///
/// Implementations may read from a local git clone, an HTTP sparse index,
/// or an in-memory store (for testing). The resolver uses this trait to
/// look up available versions during dependency resolution.
pub trait RegistryReader: Send + Sync {
    /// Get all published versions of a package, ordered oldest → newest.
    fn versions(&self, id: &ResourceId) -> Result<Vec<RegistryEntry>, RegistryError>;

    /// Get a specific version of a package.
    fn version(
        &self,
        id: &ResourceId,
        version: &str,
    ) -> Result<Option<RegistryEntry>, RegistryError> {
        let versions = self.versions(id)?;
        Ok(versions.into_iter().find(|e| e.vers == version))
    }

    /// Check whether a package exists in the index.
    fn exists(&self, id: &ResourceId) -> Result<bool, RegistryError> {
        Ok(!self.versions(id)?.is_empty())
    }
}

// ── In-memory registry (testing) ─────────────────────────────────────

/// In-memory registry for use in tests and example flows.
///
/// Stores entries in a `HashMap` keyed by `publisher/name`. Entries are
/// returned in insertion order (oldest first).
#[derive(Debug, Default)]
pub struct MemoryRegistry {
    packages: HashMap<String, Vec<RegistryEntry>>,
}

impl MemoryRegistry {
    /// Create a new empty memory registry.
    pub fn new() -> Self {
        Self::default()
    }

    /// Add an entry to the registry. The key is derived from the entry's
    /// `publisher` and `name` fields.
    pub fn add(&mut self, entry: RegistryEntry) {
        let key = format!("{}/{}", entry.publisher, entry.name);
        self.packages.entry(key).or_default().push(entry);
    }

    /// Total number of (package, version) entries across all packages.
    pub fn entry_count(&self) -> usize {
        self.packages.values().map(Vec::len).sum()
    }
}

impl RegistryReader for MemoryRegistry {
    fn versions(&self, id: &ResourceId) -> Result<Vec<RegistryEntry>, RegistryError> {
        let key = id.to_string();
        Ok(self.packages.get(&key).cloned().unwrap_or_default())
    }
}

// ── RegistryEntry → PackageManifest conversion ───────────────────────

impl RegistryEntry {
    /// The full `publisher/name` identity string.
    pub fn full_name(&self) -> String {
        format!("{}/{}", self.publisher, self.name)
    }

    /// Convert to a [`PackageManifest`] for interop with workshop-core's
    /// [`IndexBackend`](workshop_core::IndexBackend) trait.
    ///
    /// This is a lossy conversion: `yanked`, `features`, `manifest_hash`,
    /// and `links` are not represented in `PackageManifest`.
    pub fn to_package_manifest(&self) -> Result<PackageManifest, RegistryError> {
        let id = ResourceId::new(&self.publisher, &self.name).map_err(|err| {
            RegistryError::InvalidPackageName {
                name: self.full_name(),
                reason: err.to_string(),
            }
        })?;

        let version = parse_resource_version(&self.vers)?;

        // Parse the checksum: strip "sha256:" prefix if present.
        let hash_hex = self.cksum.strip_prefix("sha256:").unwrap_or(&self.cksum);
        let blob_id = BlobId::from_hex(hash_hex).ok_or_else(|| RegistryError::InvalidChecksum {
            checksum: self.cksum.clone(),
        })?;

        let deps: Vec<Dependency> = self
            .deps
            .iter()
            .filter_map(|d| {
                ResourceId::new(&d.publisher, &d.name)
                    .ok()
                    .map(|dep_id| Dependency {
                        id: dep_id,
                        version_req: d.req.clone(),
                        optional: d.optional,
                    })
            })
            .collect();

        Ok(PackageManifest::new(id, version, blob_id, 0).with_dependencies(deps))
    }
}

/// Parse a semver string into workshop-core's `ResourceVersion`.
///
/// Strips pre-release suffixes (e.g. `"1.2.3-beta.1"` → `1.2.3`)
/// because `ResourceVersion` represents release versions only.
fn parse_resource_version(version_str: &str) -> Result<ResourceVersion, RegistryError> {
    let mut parts = version_str.splitn(4, '.');
    let major_str = parts.next().ok_or_else(|| RegistryError::InvalidVersion {
        version: version_str.to_string(),
    })?;
    let minor_str = parts.next().ok_or_else(|| RegistryError::InvalidVersion {
        version: version_str.to_string(),
    })?;
    let patch_and_pre = parts.next().ok_or_else(|| RegistryError::InvalidVersion {
        version: version_str.to_string(),
    })?;

    let major: u32 = major_str
        .parse()
        .map_err(|_| RegistryError::InvalidVersion {
            version: version_str.to_string(),
        })?;
    let minor: u32 = minor_str
        .parse()
        .map_err(|_| RegistryError::InvalidVersion {
            version: version_str.to_string(),
        })?;
    // Strip pre-release suffix from patch (e.g. "3-beta.1" → "3").
    let patch_str = patch_and_pre.split('-').next().unwrap_or(patch_and_pre);
    let patch: u32 = patch_str
        .parse()
        .map_err(|_| RegistryError::InvalidVersion {
            version: version_str.to_string(),
        })?;

    Ok(ResourceVersion::new(major, minor, patch))
}

// ── Error types ──────────────────────────────────────────────────────

/// Errors from registry index operations.
#[derive(Debug, thiserror::Error)]
pub enum RegistryError {
    /// NDJSON parse failure at a specific line.
    #[error("parse error on line {line}: {message}")]
    ParseError { line: usize, message: String },

    /// Serialization failure.
    #[error("serialization error: {message}")]
    SerializeError { message: String },

    /// Invalid package name or publisher slug.
    #[error("invalid package name `{name}`: {reason}")]
    InvalidPackageName { name: String, reason: String },

    /// Invalid semver version string.
    #[error("invalid version `{version}`")]
    InvalidVersion { version: String },

    /// Invalid or unparseable checksum.
    #[error("invalid checksum `{checksum}`")]
    InvalidChecksum { checksum: String },

    /// I/O error accessing the index.
    #[error("index I/O error: {message}")]
    Io {
        message: String,
        #[source]
        source: Option<std::io::Error>,
    },
}

// ── Tests ────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;

    /// Helper: create a minimal valid registry entry for testing.
    fn test_entry(publisher: &str, name: &str, version: &str) -> RegistryEntry {
        let hash = BlobId::from_data(format!("{publisher}/{name}@{version}").as_bytes());
        RegistryEntry {
            name: name.to_string(),
            vers: version.to_string(),
            publisher: publisher.to_string(),
            deps: vec![],
            cksum: format!("sha256:{}", hash.to_hex()),
            manifest_hash: format!("sha256:{}", hash.to_hex()),
            features: HashMap::new(),
            yanked: false,
            links: None,
        }
    }

    // ── Index path sharding ──────────────────────────────────────────

    /// 1-char package names go in the `1/` directory.
    #[test]
    fn shard_path_one_char_name() {
        let path = index_shard_path("alice", "x").unwrap();
        assert_eq!(path, "1/alice/x");
    }

    /// 2-char package names go in the `2/` directory.
    #[test]
    fn shard_path_two_char_name() {
        let path = index_shard_path("alice", "ui").unwrap();
        assert_eq!(path, "2/alice/ui");
    }

    /// 3-char package names go in the `3/` directory.
    #[test]
    fn shard_path_three_char_name() {
        let path = index_shard_path("alice", "map").unwrap();
        assert_eq!(path, "3/alice/map");
    }

    /// 4+ char names use first-2/next-2 character sharding.
    ///
    /// This matches crates.io's index sharding strategy and keeps directory
    /// sizes manageable as the registry grows.
    #[test]
    fn shard_path_long_name() {
        let path = index_shard_path("community-project", "hd-infantry-sprites").unwrap();
        assert_eq!(path, "hd/-i/community-project/hd-infantry-sprites");
    }

    /// Empty package name is rejected.
    #[test]
    fn shard_path_empty_name_rejected() {
        assert!(index_shard_path("alice", "").is_err());
    }

    /// Empty publisher is rejected.
    #[test]
    fn shard_path_empty_publisher_rejected() {
        assert!(index_shard_path("", "sprites").is_err());
    }

    // ── NDJSON parsing ───────────────────────────────────────────────

    /// A well-formed single-line NDJSON entry parses correctly.
    #[test]
    fn parse_single_entry() {
        let entry = test_entry("alice", "sprites", "1.0.0");
        let json = serde_json::to_string(&entry).unwrap();
        let parsed = parse_index_file(&json).unwrap();
        assert_eq!(parsed.len(), 1);
        assert_eq!(parsed[0].name, "sprites");
        assert_eq!(parsed[0].vers, "1.0.0");
        assert_eq!(parsed[0].publisher, "alice");
    }

    /// Blank lines and comment lines are silently skipped.
    ///
    /// Index files may contain comments (prefixed with `#`) for
    /// human-readable annotations. The parser ignores them.
    #[test]
    fn parse_skips_blanks_and_comments() {
        let entry = test_entry("alice", "sprites", "1.0.0");
        let json = serde_json::to_string(&entry).unwrap();
        let content = format!("# This is a comment\n\n{json}\n  \n");
        let parsed = parse_index_file(&content).unwrap();
        assert_eq!(parsed.len(), 1);
    }

    /// Multiple NDJSON lines parse as separate entries in file order.
    #[test]
    fn parse_multiple_entries_preserves_order() {
        let e1 = test_entry("alice", "sprites", "1.0.0");
        let e2 = test_entry("alice", "sprites", "1.1.0");
        let e3 = test_entry("alice", "sprites", "2.0.0");
        let content = [&e1, &e2, &e3]
            .iter()
            .map(|e| serde_json::to_string(e).unwrap())
            .collect::<Vec<_>>()
            .join("\n");
        let parsed = parse_index_file(&content).unwrap();
        assert_eq!(parsed.len(), 3);
        assert_eq!(parsed[0].vers, "1.0.0");
        assert_eq!(parsed[1].vers, "1.1.0");
        assert_eq!(parsed[2].vers, "2.0.0");
    }

    /// Invalid JSON on a line produces a ParseError with the line number.
    #[test]
    fn parse_invalid_json_reports_line() {
        let content = "not valid json";
        let err = parse_index_file(content).unwrap_err();
        match err {
            RegistryError::ParseError { line, .. } => assert_eq!(line, 1),
            other => panic!("expected ParseError, got {other}"),
        }
    }

    // ── Serialization round-trip ─────────────────────────────────────

    /// An entry serialized then parsed back is structurally identical.
    ///
    /// Deterministic serialization is critical for git-based indexes where
    /// diff minimization matters.
    #[test]
    fn serialize_round_trip() {
        let original = test_entry("alice", "sprites", "1.0.0");
        let json = serialize_entry(&original).unwrap();
        let parsed: RegistryEntry = serde_json::from_str(&json).unwrap();
        assert_eq!(parsed.name, original.name);
        assert_eq!(parsed.vers, original.vers);
        assert_eq!(parsed.publisher, original.publisher);
        assert_eq!(parsed.cksum, original.cksum);
    }

    // ── MemoryRegistry ───────────────────────────────────────────────

    /// Adding entries and retrieving them by ResourceId works correctly.
    #[test]
    fn memory_registry_add_and_retrieve() {
        let mut reg = MemoryRegistry::new();
        reg.add(test_entry("alice", "sprites", "1.0.0"));
        reg.add(test_entry("alice", "sprites", "1.1.0"));

        let id = ResourceId::new("alice", "sprites").unwrap();
        let versions = reg.versions(&id).unwrap();
        assert_eq!(versions.len(), 2);
        assert_eq!(reg.entry_count(), 2);
    }

    /// Querying an unknown package returns an empty list, not an error.
    ///
    /// An empty result is not a failure — it means the package hasn't been
    /// published. The resolver uses this to report `PackageNotFound`.
    #[test]
    fn memory_registry_unknown_package_returns_empty() {
        let reg = MemoryRegistry::new();
        let id = ResourceId::new("alice", "nonexistent").unwrap();
        let versions = reg.versions(&id).unwrap();
        assert!(versions.is_empty());
    }

    /// The `exists` method returns false for unknown packages.
    #[test]
    fn memory_registry_exists_false_for_missing() {
        let reg = MemoryRegistry::new();
        let id = ResourceId::new("alice", "nonexistent").unwrap();
        assert!(!reg.exists(&id).unwrap());
    }

    // ── PackageManifest conversion ───────────────────────────────────

    /// A valid entry converts to PackageManifest with correct fields.
    ///
    /// This conversion bridges the registry layer to workshop-core's
    /// IndexBackend interface, enabling the failover index to serve
    /// registry entries as PackageManifests.
    #[test]
    fn entry_to_package_manifest() {
        let entry = test_entry("alice", "sprites", "1.2.3");
        let manifest = entry.to_package_manifest().unwrap();
        assert_eq!(manifest.id().publisher(), "alice");
        assert_eq!(manifest.id().name(), "sprites");
        assert_eq!(manifest.version(), ResourceVersion::new(1, 2, 3));
    }

    /// Pre-release suffixes are stripped during version conversion.
    #[test]
    fn entry_version_strips_prerelease() {
        let mut entry = test_entry("alice", "sprites", "2.0.0-beta.1");
        entry.vers = "2.0.0-beta.1".to_string();
        let manifest = entry.to_package_manifest().unwrap();
        assert_eq!(manifest.version(), ResourceVersion::new(2, 0, 0));
    }

    // ── Error display ────────────────────────────────────────────────

    /// ParseError display includes line number and message.
    #[test]
    fn error_display_parse_error() {
        let err = RegistryError::ParseError {
            line: 42,
            message: "unexpected token".to_string(),
        };
        let msg = err.to_string();
        assert!(msg.contains("42"), "{msg}");
        assert!(msg.contains("unexpected token"), "{msg}");
    }

    /// InvalidChecksum display includes the bad checksum value.
    #[test]
    fn error_display_invalid_checksum() {
        let err = RegistryError::InvalidChecksum {
            checksum: "not-a-hash".to_string(),
        };
        assert!(err.to_string().contains("not-a-hash"));
    }
}
