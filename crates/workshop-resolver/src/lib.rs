// SPDX-License-Identifier: MIT OR Apache-2.0

//! Dependency resolution and lock file management for Workshop packages.
//!
//! This crate resolves `mod.toml` dependency declarations against a
//! [`RegistryReader`](workshop_registry::RegistryReader) and produces a
//! deterministic lock file (`ic.lock`) that pins exact versions, sources,
//! and checksums for reproducible installs.
//!
//! # Architecture
//!
//! The resolver sits between the manifest (`mod.toml`) and the registry
//! index. It reads dependency declarations, queries the registry for
//! available versions, and selects the newest compatible version for each
//! package. The result is a [`LockFile`] that downstream tools use for
//! deterministic installs.
//!
//! ```text
//! mod.toml (DependencySpec[])
//!       │
//!       ▼
//! workshop-resolver  ◄── workshop-registry (RegistryReader)
//!       │
//!       ▼
//! ic.lock (LockFile)
//!       │
//!       ▼
//! workshop-package + p2p-distribute (download & install)
//! ```
//!
//! # Current status
//!
//! The current resolver handles direct dependencies with a greedy
//! newest-compatible strategy. Transitive dependency resolution with
//! diamond conflict handling will be added via PubGrub integration —
//! see `dependency-resolution-design.md` §5 for the full
//! `DependencyProvider` implementation plan.
//!
//! # Design authority
//!
//! - `dependency-resolution-design.md` — algorithm, lock file format, error reporting
//! - D030 — version immutability, yanking semantics
//! - D067 — `mod.toml` manifest format

use workshop_core::ResourceId;
use workshop_registry::{RegistryError, RegistryReader};

// ── Dependency specification (from mod.toml) ─────────────────────────

/// A dependency declaration from the root manifest (`mod.toml`).
///
/// This is what the modder writes. The resolver converts these
/// declarations into pinned [`ResolvedPackage`] entries in the lock file.
#[derive(Debug, Clone)]
pub struct DependencySpec {
    /// The package being depended on (`publisher/name`).
    pub package: ResourceId,
    /// Semver version requirement (e.g. `"^1.2"`, `">=2.0, <3.0"`).
    pub version_req: String,
    /// Where to fetch this dependency from.
    pub source: DependencySource,
    /// Whether this dependency is optional (feature-gated).
    pub optional: bool,
}

/// Where a dependency is fetched from.
///
/// Workshop is the primary source. Other sources exist for development
/// workflows (local paths, git repos) and emergency pinning (direct URLs).
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum DependencySource {
    /// Workshop registry (official or federated).
    Workshop {
        /// Optional alternate registry URL. `None` means the default registry.
        registry_url: Option<String>,
    },
    /// Local path on disk (development only, not publishable).
    Local { path: String },
    /// Git repository.
    Git {
        url: String,
        reference: GitReference,
    },
    /// Direct URL download (pinned by checksum).
    Url { url: String, checksum: String },
}

/// Git reference for a git-sourced dependency.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum GitReference {
    Branch(String),
    Tag(String),
    Rev(String),
    DefaultBranch,
}

// ── Lock file types ──────────────────────────────────────────────────

/// The complete lock file (`ic.lock`) — deterministic resolution output.
///
/// Records every resolved package with exact versions, sources, and
/// checksums. Two developers with the same `mod.toml` and same registry
/// state will produce byte-identical lock files.
///
/// # Format
///
/// TOML, consistent with Cargo's `Cargo.lock`:
/// ```toml
/// [metadata]
/// ic_lock_version = 1
/// generated_by = "workshop-resolver 0.1.0-alpha.0"
/// index_commit = "a1b2c3d4..."
///
/// [[package]]
/// name = "alice/sprites"
/// version = "1.2.3"
/// source = "workshop+https://workshop.ironcurtain.gg"
/// checksum = "sha256:abcdef..."
/// dependencies = []
/// ```
#[derive(Debug, Clone, serde::Deserialize, serde::Serialize)]
pub struct LockFile {
    /// Lock file metadata (format version, generation info, index state).
    pub metadata: LockMetadata,
    /// Resolved packages in dependency-sorted order.
    #[serde(default)]
    pub package: Vec<ResolvedPackage>,
}

/// Lock file metadata section.
#[derive(Debug, Clone, serde::Deserialize, serde::Serialize)]
pub struct LockMetadata {
    /// Lock file format version (currently `1`). Enables future migrations.
    pub ic_lock_version: u32,
    /// Tool version that generated this lock file.
    pub generated_by: String,
    /// ISO 8601 timestamp of generation.
    pub generated_at: String,
    /// Git commit hash of the workshop-index at resolution time.
    pub index_commit: String,
    /// URL of the git-index repository used for resolution.
    pub index_url: String,
}

/// A fully resolved package in the lock file.
///
/// Each entry records the exact version selected by the resolver, its
/// download source (with scheme prefix for dependency confusion prevention),
/// and the SHA-256 checksum for integrity verification.
#[derive(Debug, Clone, serde::Deserialize, serde::Serialize)]
pub struct ResolvedPackage {
    /// Full `publisher/name` identity.
    pub name: String,
    /// Exact semver version.
    pub version: String,
    /// Source URL with scheme prefix (`workshop+`, `git+`, `path+`).
    pub source: String,
    /// `sha256:` prefixed hex digest of the package archive.
    pub checksum: String,
    /// Dependencies as `publisher/name` strings (versions in their own entries).
    #[serde(default)]
    pub dependencies: Vec<String>,
}

impl LockFile {
    /// Parse a lock file from TOML content.
    pub fn from_toml(content: &str) -> Result<Self, ResolutionError> {
        toml::from_str(content).map_err(|err| ResolutionError::LockFileParse {
            message: err.to_string(),
        })
    }

    /// Serialize to TOML for writing to disk.
    pub fn to_toml(&self) -> Result<String, ResolutionError> {
        toml::to_string_pretty(self).map_err(|err| ResolutionError::LockFileSerialize {
            message: err.to_string(),
        })
    }

    /// Look up a locked version for a package by name.
    pub fn locked_version(&self, name: &str) -> Option<&ResolvedPackage> {
        self.package.iter().find(|p| p.name == name)
    }

    /// Check if the lock file is consistent with a set of manifest dependencies.
    ///
    /// Returns [`ConsistencyCheck::Consistent`] if every dependency in the
    /// manifest is represented in the lock file with a compatible version.
    /// Used by `ic mod install --locked` to reject out-of-date lock files.
    pub fn is_consistent_with(&self, manifest_deps: &[DependencySpec]) -> ConsistencyCheck {
        let mut added = Vec::new();
        let mut removed = Vec::new();
        let mut changed = Vec::new();

        let locked_names: std::collections::HashSet<&str> =
            self.package.iter().map(|p| p.name.as_str()).collect();
        let manifest_names: std::collections::HashSet<String> = manifest_deps
            .iter()
            .map(|d| d.package.to_string())
            .collect();

        for dep in manifest_deps {
            let full_name = dep.package.to_string();
            if !locked_names.contains(full_name.as_str()) {
                added.push(full_name);
            } else if let Some(locked) = self.locked_version(&dep.package.to_string()) {
                // Check if the locked version still satisfies the requirement.
                if let Ok(req) = semver::VersionReq::parse(&dep.version_req) {
                    if let Ok(ver) = semver::Version::parse(&locked.version) {
                        if !req.matches(&ver) {
                            changed.push((full_name, dep.version_req.clone()));
                        }
                    }
                }
            }
        }

        for pkg in &self.package {
            if !manifest_names.contains(&pkg.name) {
                removed.push(pkg.name.clone());
            }
        }

        if added.is_empty() && removed.is_empty() && changed.is_empty() {
            ConsistencyCheck::Consistent
        } else {
            ConsistencyCheck::Inconsistent {
                added,
                removed,
                changed,
            }
        }
    }
}

/// Result of checking lock file consistency with a manifest.
#[derive(Debug)]
pub enum ConsistencyCheck {
    /// Lock file matches the manifest — safe to install.
    Consistent,
    /// Lock file diverges from the manifest.
    Inconsistent {
        /// Dependencies in the manifest but not in the lock file.
        added: Vec<String>,
        /// Packages in the lock file but not in the manifest.
        removed: Vec<String>,
        /// Dependencies whose version requirement changed.
        changed: Vec<(String, String)>,
    },
}

// ── Resolver configuration ───────────────────────────────────────────

/// Configuration for a resolution run.
pub struct ResolveConfig {
    /// URL of the registry (used in lock file `source` field).
    pub registry_url: String,
    /// Git commit hash of the index at resolution time.
    pub index_commit: String,
    /// URL of the git-index repository.
    pub index_url: String,
    /// ISO 8601 timestamp for the lock file.
    pub generated_at: String,
}

// ── Resolver ─────────────────────────────────────────────────────────

/// Resolve direct dependencies to their newest compatible versions.
///
/// Queries the provided [`RegistryReader`] for each dependency and selects
/// the newest non-yanked version satisfying the semver requirement.
///
/// # Current limitations
///
/// This is a direct-dependencies-only resolver. It does **not** resolve
/// transitive dependencies or handle diamond conflicts. Full transitive
/// resolution with backtracking will be added via PubGrub integration
/// (see `dependency-resolution-design.md` §5).
///
/// # Errors
///
/// Returns [`ResolutionError::PackageNotFound`] if a dependency has no
/// versions in the registry, or [`ResolutionError::NoMatchingVersion`]
/// if no version satisfies the requirement.
pub fn resolve(
    deps: &[DependencySpec],
    registry: &dyn RegistryReader,
    config: &ResolveConfig,
) -> Result<LockFile, ResolutionError> {
    let mut resolved = Vec::with_capacity(deps.len());

    for spec in deps {
        // Skip non-workshop sources — local/git/url are handled by the
        // install step directly, not through registry resolution.
        if !matches!(spec.source, DependencySource::Workshop { .. }) {
            continue;
        }

        let entries =
            registry
                .versions(&spec.package)
                .map_err(|err| ResolutionError::RegistryError {
                    message: err.to_string(),
                })?;

        if entries.is_empty() {
            return Err(ResolutionError::PackageNotFound {
                package: spec.package.to_string(),
            });
        }

        let req = semver::VersionReq::parse(&spec.version_req).map_err(|err| {
            ResolutionError::InvalidVersionReq {
                requirement: spec.version_req.clone(),
                reason: err.to_string(),
            }
        })?;

        // Find the newest non-yanked version matching the requirement.
        // Entries are oldest-first (append-only NDJSON), so iterate in
        // reverse to find newest first.
        let matching = entries.iter().rev().filter(|e| !e.yanked).find(|e| {
            semver::Version::parse(&e.vers)
                .map(|v| req.matches(&v))
                .unwrap_or(false)
        });

        let entry = matching.ok_or_else(|| {
            let available: Vec<String> = entries.iter().map(|e| e.vers.clone()).collect();
            ResolutionError::NoMatchingVersion {
                package: spec.package.to_string(),
                requirement: spec.version_req.clone(),
                available,
            }
        })?;

        resolved.push(ResolvedPackage {
            name: entry.full_name(),
            version: entry.vers.clone(),
            source: format!("workshop+{}", config.registry_url),
            checksum: entry.cksum.clone(),
            dependencies: entry
                .deps
                .iter()
                .map(|d| format!("{}/{}", d.publisher, d.name))
                .collect(),
        });
    }

    Ok(LockFile {
        metadata: LockMetadata {
            ic_lock_version: 1,
            generated_by: "workshop-resolver 0.1.0-alpha.0".to_string(),
            generated_at: config.generated_at.clone(),
            index_commit: config.index_commit.clone(),
            index_url: config.index_url.clone(),
        },
        package: resolved,
    })
}

// ── Error types ──────────────────────────────────────────────────────

/// Errors from the dependency resolution process.
#[derive(Debug, thiserror::Error)]
pub enum ResolutionError {
    /// A dependency was not found in the registry at all.
    #[error("package not found in registry: {package}")]
    PackageNotFound { package: String },

    /// No version of a package satisfies the declared requirement.
    #[error("no version of {package} matches {requirement} (available: {})",
        available.join(", "))]
    NoMatchingVersion {
        package: String,
        requirement: String,
        available: Vec<String>,
    },

    /// A version requirement string could not be parsed as semver.
    #[error("invalid version requirement `{requirement}`: {reason}")]
    InvalidVersionReq { requirement: String, reason: String },

    /// The registry backend returned an error.
    #[error("registry error: {message}")]
    RegistryError { message: String },

    /// Lock file TOML could not be parsed.
    #[error("lock file parse error: {message}")]
    LockFileParse { message: String },

    /// Lock file could not be serialized to TOML.
    #[error("lock file serialization error: {message}")]
    LockFileSerialize { message: String },
}

impl From<RegistryError> for ResolutionError {
    fn from(err: RegistryError) -> Self {
        Self::RegistryError {
            message: err.to_string(),
        }
    }
}

// ── Tests ────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;
    use workshop_core::BlobId;
    use workshop_registry::{MemoryRegistry, RegistryEntry};

    /// Helper: create a test registry entry.
    fn entry(publisher: &str, name: &str, version: &str) -> RegistryEntry {
        let hash = BlobId::from_data(format!("{publisher}/{name}@{version}").as_bytes());
        RegistryEntry {
            name: name.to_string(),
            vers: version.to_string(),
            publisher: publisher.to_string(),
            deps: vec![],
            cksum: format!("sha256:{}", hash.to_hex()),
            manifest_hash: format!("sha256:{}", hash.to_hex()),
            features: std::collections::HashMap::new(),
            yanked: false,
            links: None,
        }
    }

    /// Helper: create a default ResolveConfig for tests.
    fn test_config() -> ResolveConfig {
        ResolveConfig {
            registry_url: "https://workshop.test".to_string(),
            index_commit: "abc123".to_string(),
            index_url: "https://index.test".to_string(),
            generated_at: "2026-04-11T00:00:00Z".to_string(),
        }
    }

    /// Helper: create a workshop DependencySpec.
    fn dep(publisher: &str, name: &str, req: &str) -> DependencySpec {
        DependencySpec {
            package: ResourceId::new(publisher, name).unwrap(),
            version_req: req.to_string(),
            source: DependencySource::Workshop { registry_url: None },
            optional: false,
        }
    }

    // ── Lock file TOML round-trip ────────────────────────────────────

    /// A lock file serialized to TOML and parsed back is structurally identical.
    ///
    /// Deterministic serialization is essential: `ic mod install --locked`
    /// must be able to read any lock file written by any resolver version.
    #[test]
    fn lock_file_toml_round_trip() {
        let lock = LockFile {
            metadata: LockMetadata {
                ic_lock_version: 1,
                generated_by: "test".to_string(),
                generated_at: "2026-01-01T00:00:00Z".to_string(),
                index_commit: "deadbeef".to_string(),
                index_url: "https://index.test".to_string(),
            },
            package: vec![ResolvedPackage {
                name: "alice/sprites".to_string(),
                version: "1.2.3".to_string(),
                source: "workshop+https://workshop.test".to_string(),
                checksum: "sha256:abcdef1234".to_string(),
                dependencies: vec!["bob/palette".to_string()],
            }],
        };

        let toml_str = lock.to_toml().unwrap();
        let parsed = LockFile::from_toml(&toml_str).unwrap();

        assert_eq!(parsed.metadata.ic_lock_version, 1);
        assert_eq!(parsed.package.len(), 1);
        assert_eq!(parsed.package[0].name, "alice/sprites");
        assert_eq!(parsed.package[0].version, "1.2.3");
        assert_eq!(parsed.package[0].dependencies, vec!["bob/palette"]);
    }

    // ── Lock file consistency checks ─────────────────────────────────

    /// A lock file matching the manifest is reported as consistent.
    #[test]
    fn lock_file_consistent_when_matching() {
        let lock = LockFile {
            metadata: LockMetadata {
                ic_lock_version: 1,
                generated_by: "test".to_string(),
                generated_at: String::new(),
                index_commit: String::new(),
                index_url: String::new(),
            },
            package: vec![ResolvedPackage {
                name: "alice/sprites".to_string(),
                version: "1.5.0".to_string(),
                source: "workshop+https://test".to_string(),
                checksum: "sha256:abc".to_string(),
                dependencies: vec![],
            }],
        };

        let deps = vec![dep("alice", "sprites", "^1.0")];
        assert!(matches!(
            lock.is_consistent_with(&deps),
            ConsistencyCheck::Consistent
        ));
    }

    /// Adding a dependency to mod.toml that isn't in the lock file triggers inconsistency.
    #[test]
    fn lock_file_detects_added_dependency() {
        let lock = LockFile {
            metadata: LockMetadata {
                ic_lock_version: 1,
                generated_by: "test".to_string(),
                generated_at: String::new(),
                index_commit: String::new(),
                index_url: String::new(),
            },
            package: vec![],
        };

        let deps = vec![dep("alice", "sprites", "^1.0")];
        match lock.is_consistent_with(&deps) {
            ConsistencyCheck::Inconsistent { added, .. } => {
                assert_eq!(added, vec!["alice/sprites"]);
            }
            ConsistencyCheck::Consistent => panic!("expected inconsistent"),
        }
    }

    /// A package in the lock file but not in the manifest is detected as removed.
    #[test]
    fn lock_file_detects_removed_dependency() {
        let lock = LockFile {
            metadata: LockMetadata {
                ic_lock_version: 1,
                generated_by: "test".to_string(),
                generated_at: String::new(),
                index_commit: String::new(),
                index_url: String::new(),
            },
            package: vec![ResolvedPackage {
                name: "alice/sprites".to_string(),
                version: "1.0.0".to_string(),
                source: "workshop+https://test".to_string(),
                checksum: "sha256:abc".to_string(),
                dependencies: vec![],
            }],
        };

        let deps: Vec<DependencySpec> = vec![];
        match lock.is_consistent_with(&deps) {
            ConsistencyCheck::Inconsistent { removed, .. } => {
                assert_eq!(removed, vec!["alice/sprites"]);
            }
            ConsistencyCheck::Consistent => panic!("expected inconsistent"),
        }
    }

    // ── Resolution: happy path ───────────────────────────────────────

    /// The resolver picks the newest compatible version for a single dep.
    ///
    /// Given versions 1.0.0, 1.1.0, 1.2.0, and 2.0.0, a `^1.0` requirement
    /// should select 1.2.0 (newest within the 1.x range).
    #[test]
    fn resolve_picks_newest_compatible() {
        let mut reg = MemoryRegistry::new();
        reg.add(entry("alice", "sprites", "1.0.0"));
        reg.add(entry("alice", "sprites", "1.1.0"));
        reg.add(entry("alice", "sprites", "1.2.0"));
        reg.add(entry("alice", "sprites", "2.0.0"));

        let deps = vec![dep("alice", "sprites", "^1.0")];
        let lock = resolve(&deps, &reg, &test_config()).unwrap();

        assert_eq!(lock.package.len(), 1);
        assert_eq!(lock.package[0].name, "alice/sprites");
        assert_eq!(lock.package[0].version, "1.2.0");
    }

    /// Multiple direct dependencies are each resolved independently.
    #[test]
    fn resolve_multiple_deps() {
        let mut reg = MemoryRegistry::new();
        reg.add(entry("alice", "sprites", "1.0.0"));
        reg.add(entry("bob", "music", "2.3.0"));

        let deps = vec![dep("alice", "sprites", "^1.0"), dep("bob", "music", "^2.0")];
        let lock = resolve(&deps, &reg, &test_config()).unwrap();

        assert_eq!(lock.package.len(), 2);
    }

    // ── Resolution: error paths ──────────────────────────────────────

    /// Resolution fails with PackageNotFound for an unknown package.
    #[test]
    fn resolve_package_not_found() {
        let reg = MemoryRegistry::new();
        let deps = vec![dep("alice", "nonexistent", "^1.0")];

        let err = resolve(&deps, &reg, &test_config()).unwrap_err();
        assert!(matches!(err, ResolutionError::PackageNotFound { .. }));
        assert!(err.to_string().contains("alice/nonexistent"));
    }

    /// Resolution fails when no version matches the requirement.
    #[test]
    fn resolve_no_matching_version() {
        let mut reg = MemoryRegistry::new();
        reg.add(entry("alice", "sprites", "1.0.0"));

        let deps = vec![dep("alice", "sprites", "^2.0")];
        let err = resolve(&deps, &reg, &test_config()).unwrap_err();

        assert!(matches!(err, ResolutionError::NoMatchingVersion { .. }));
        assert!(err.to_string().contains("1.0.0"), "{}", err);
    }

    /// Yanked versions are excluded from resolution.
    ///
    /// A yanked version is still in the index but should not be selected
    /// for new installs. Only locked versions may use yanked packages.
    #[test]
    fn resolve_skips_yanked_versions() {
        let mut reg = MemoryRegistry::new();
        reg.add(entry("alice", "sprites", "1.0.0"));

        let mut yanked = entry("alice", "sprites", "1.1.0");
        yanked.yanked = true;
        reg.add(yanked);

        let deps = vec![dep("alice", "sprites", "^1.0")];
        let lock = resolve(&deps, &reg, &test_config()).unwrap();

        // Should pick 1.0.0, skipping yanked 1.1.0.
        assert_eq!(lock.package[0].version, "1.0.0");
    }

    /// An invalid version requirement string produces a clear error.
    #[test]
    fn resolve_invalid_version_req() {
        let mut reg = MemoryRegistry::new();
        reg.add(entry("alice", "sprites", "1.0.0"));

        let deps = vec![dep("alice", "sprites", "not-semver!!!")];
        let err = resolve(&deps, &reg, &test_config()).unwrap_err();

        assert!(matches!(err, ResolutionError::InvalidVersionReq { .. }));
    }

    // ── Resolution: lock file metadata ───────────────────────────────

    /// The lock file records the index commit and registry URL from config.
    ///
    /// These metadata fields enable deterministic reproduction: anyone with
    /// the same index commit and the same mod.toml gets the same lock file.
    #[test]
    fn resolve_records_metadata() {
        let mut reg = MemoryRegistry::new();
        reg.add(entry("alice", "sprites", "1.0.0"));

        let config = ResolveConfig {
            registry_url: "https://custom.registry".to_string(),
            index_commit: "cafebabe".to_string(),
            index_url: "https://index.custom".to_string(),
            generated_at: "2026-04-11T12:00:00Z".to_string(),
        };

        let deps = vec![dep("alice", "sprites", "^1.0")];
        let lock = resolve(&deps, &reg, &config).unwrap();

        assert_eq!(lock.metadata.index_commit, "cafebabe");
        assert_eq!(lock.metadata.index_url, "https://index.custom");
        assert_eq!(lock.metadata.ic_lock_version, 1);
        assert!(lock.package[0].source.contains("https://custom.registry"));
    }

    // ── Error display ────────────────────────────────────────────────

    /// NoMatchingVersion display includes the package and available versions.
    #[test]
    fn error_display_no_matching_version() {
        let err = ResolutionError::NoMatchingVersion {
            package: "alice/sprites".to_string(),
            requirement: "^3.0".to_string(),
            available: vec!["1.0.0".to_string(), "2.0.0".to_string()],
        };
        let msg = err.to_string();
        assert!(msg.contains("alice/sprites"), "{msg}");
        assert!(msg.contains("^3.0"), "{msg}");
        assert!(msg.contains("1.0.0"), "{msg}");
    }

    /// PackageNotFound display includes the package name.
    #[test]
    fn error_display_package_not_found() {
        let err = ResolutionError::PackageNotFound {
            package: "alice/nonexistent".to_string(),
        };
        assert!(err.to_string().contains("alice/nonexistent"));
    }
}
