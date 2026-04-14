// SPDX-License-Identifier: MIT OR Apache-2.0

//! Resource identity and versioning types.
//!
//! Every Workshop resource is globally identified by a `publisher/name@version`
//! triple (D030). Publishers are lowercase alphanumeric with hyphens, names
//! follow the same rules, and versions use semantic versioning. This identity
//! scheme is game-agnostic — Iron Curtain, an XCOM clone, and a Civ clone all
//! use the same `ResourceId` type (D050).
//!
//! Version immutability (D030): once a `publisher/name@version` is published,
//! its content hash is fixed forever. "Updating" means publishing a new version,
//! never mutating an existing one.

use std::fmt;

use crate::error::WorkshopError;

// ── Resource identity ────────────────────────────────────────────────

/// A globally unique resource identifier: `publisher/name`.
///
/// Does not include version — this identifies the *resource series*, not a
/// specific release. Combine with [`ResourceVersion`] for a fully-qualified
/// identity.
///
/// # Invariants
///
/// Publisher and name are both lowercase ASCII alphanumeric with hyphens,
/// 1–64 characters, no leading/trailing hyphens, no consecutive hyphens.
/// These rules prevent typosquatting via case folding or Unicode tricks
/// (D030 WIDX-001).
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct ResourceId {
    publisher: String,
    name: String,
}

impl ResourceId {
    /// Creates a new resource identifier after validating both components.
    pub fn new(publisher: &str, name: &str) -> Result<Self, WorkshopError> {
        validate_slug(publisher, "publisher")?;
        validate_slug(name, "name")?;
        Ok(Self {
            publisher: publisher.to_string(),
            name: name.to_string(),
        })
    }

    /// Returns the publisher component.
    pub fn publisher(&self) -> &str {
        &self.publisher
    }

    /// Returns the name component.
    pub fn name(&self) -> &str {
        &self.name
    }
}

impl fmt::Display for ResourceId {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}/{}", self.publisher, self.name)
    }
}

// ── Semantic version ─────────────────────────────────────────────────

/// A semantic version (major.minor.patch).
///
/// Follows semver 2.0.0 rules. Pre-release and build metadata are
/// intentionally omitted for V1 — Workshop versions are release-only.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct ResourceVersion {
    major: u32,
    minor: u32,
    patch: u32,
}

impl ResourceVersion {
    /// Creates a new version.
    pub const fn new(major: u32, minor: u32, patch: u32) -> Self {
        Self {
            major,
            minor,
            patch,
        }
    }

    pub const fn major(&self) -> u32 {
        self.major
    }

    pub const fn minor(&self) -> u32 {
        self.minor
    }

    pub const fn patch(&self) -> u32 {
        self.patch
    }
}

impl fmt::Display for ResourceVersion {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}.{}.{}", self.major, self.minor, self.patch)
    }
}

// ── Dependency declaration ───────────────────────────────────────────

/// A dependency on another Workshop resource.
///
/// The `version_req` field uses caret ranges by default (e.g. `^1.2.0`
/// matches `>=1.2.0, <2.0.0`), consistent with Cargo/npm semver
/// conventions. Resolution uses the PubGrub algorithm (D030).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Dependency {
    /// The resource being depended on (`publisher/name`).
    pub id: ResourceId,
    /// Semver version requirement string (e.g. `"^1.2"`, `">=2.0, <3.0"`).
    pub version_req: String,
    /// Whether this dependency is optional (feature-gated by the consumer).
    pub optional: bool,
}

// ── Promotion channels ───────────────────────────────────────────────

/// Promotion maturity channel for a package release (D030).
///
/// Resources progress through channels: `dev` → `beta` → `release`.
/// Players choose which channels they subscribe to. Servers can require
/// `release`-channel resources only.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Channel {
    /// Local development only. Not visible to other users.
    Dev,
    /// Public testing. Visible to users who opt in to beta content.
    Beta,
    /// Stable release. Default visibility for all users.
    Release,
}

impl fmt::Display for Channel {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Dev => write!(f, "dev"),
            Self::Beta => write!(f, "beta"),
            Self::Release => write!(f, "release"),
        }
    }
}

// ── Resource categories ──────────────────────────────────────────────

/// Free-form resource category tag (D050).
///
/// The core library uses tags rather than a fixed enum so that each game
/// project can define its own vocabulary without modifying this crate.
/// Common conventions are documented but not enforced here.
///
/// Common tags: `map`, `mod`, `music`, `sprites`, `voice`, `campaign`,
/// `total-conversion`, `balance`, `ui`, `shader`, `terrain`, `model`.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct ResourceCategory(String);

impl ResourceCategory {
    /// Creates a new category tag. Tags are lowercased and trimmed.
    pub fn new(tag: &str) -> Self {
        Self(tag.trim().to_ascii_lowercase())
    }

    /// Returns the tag string.
    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl fmt::Display for ResourceCategory {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}", self.0)
    }
}

// ── Slug validation ──────────────────────────────────────────────────

/// Maximum length for publisher and name slugs.
const MAX_SLUG_LEN: usize = 64;

/// Validates a slug component (publisher or name).
///
/// Rules: lowercase ASCII alphanumeric + hyphens, 1–64 chars, no
/// leading/trailing hyphens, no consecutive hyphens. These constraints
/// prevent typosquatting and ensure clean URL/path/filename embedding.
fn validate_slug(slug: &str, field: &str) -> Result<(), WorkshopError> {
    if slug.is_empty() {
        return Err(WorkshopError::InvalidIdentifier {
            field: field.to_string(),
            value: slug.to_string(),
            reason: "must not be empty".to_string(),
        });
    }
    if slug.len() > MAX_SLUG_LEN {
        return Err(WorkshopError::InvalidIdentifier {
            field: field.to_string(),
            value: slug.to_string(),
            reason: format!("exceeds maximum length of {MAX_SLUG_LEN}"),
        });
    }
    if !slug
        .bytes()
        .all(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || b == b'-')
    {
        return Err(WorkshopError::InvalidIdentifier {
            field: field.to_string(),
            value: slug.to_string(),
            reason: "must contain only lowercase ASCII letters, digits, and hyphens".to_string(),
        });
    }
    if slug.starts_with('-') || slug.ends_with('-') {
        return Err(WorkshopError::InvalidIdentifier {
            field: field.to_string(),
            value: slug.to_string(),
            reason: "must not start or end with a hyphen".to_string(),
        });
    }
    if slug.contains("--") {
        return Err(WorkshopError::InvalidIdentifier {
            field: field.to_string(),
            value: slug.to_string(),
            reason: "must not contain consecutive hyphens".to_string(),
        });
    }
    Ok(())
}

// ── Tests ────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;

    // ── ResourceId construction ──────────────────────────────────────

    /// Valid publisher/name pair creates a ResourceId successfully.
    #[test]
    fn resource_id_valid() {
        let id = ResourceId::new("community", "hd-infantry-sprites").unwrap();
        assert_eq!(id.publisher(), "community");
        assert_eq!(id.name(), "hd-infantry-sprites");
        assert_eq!(id.to_string(), "community/hd-infantry-sprites");
    }

    /// Empty publisher is rejected.
    #[test]
    fn resource_id_empty_publisher() {
        let err = ResourceId::new("", "name").unwrap_err();
        assert!(matches!(err, WorkshopError::InvalidIdentifier { .. }));
    }

    /// Uppercase characters are rejected (prevents case-folding typosquatting).
    #[test]
    fn resource_id_uppercase_rejected() {
        let err = ResourceId::new("Community", "sprites").unwrap_err();
        assert!(matches!(err, WorkshopError::InvalidIdentifier { .. }));
    }

    /// Leading hyphen is rejected.
    #[test]
    fn resource_id_leading_hyphen() {
        let err = ResourceId::new("-bad", "name").unwrap_err();
        assert!(matches!(err, WorkshopError::InvalidIdentifier { .. }));
    }

    /// Trailing hyphen is rejected.
    #[test]
    fn resource_id_trailing_hyphen() {
        let err = ResourceId::new("pub", "name-").unwrap_err();
        assert!(matches!(err, WorkshopError::InvalidIdentifier { .. }));
    }

    /// Consecutive hyphens are rejected.
    #[test]
    fn resource_id_double_hyphen() {
        let err = ResourceId::new("pub", "bad--name").unwrap_err();
        assert!(matches!(err, WorkshopError::InvalidIdentifier { .. }));
    }

    /// Slug longer than 64 characters is rejected.
    #[test]
    fn resource_id_too_long() {
        let long = "a".repeat(65);
        let err = ResourceId::new(&long, "name").unwrap_err();
        assert!(matches!(err, WorkshopError::InvalidIdentifier { .. }));
    }

    /// Special characters (underscore, dot, space) are rejected.
    #[test]
    fn resource_id_special_chars_rejected() {
        assert!(ResourceId::new("pub", "has_underscore").is_err());
        assert!(ResourceId::new("pub", "has.dot").is_err());
        assert!(ResourceId::new("pub", "has space").is_err());
    }

    // ── ResourceVersion ordering ─────────────────────────────────────

    /// Versions compare correctly following semver precedence.
    #[test]
    fn version_ordering() {
        let v100 = ResourceVersion::new(1, 0, 0);
        let v110 = ResourceVersion::new(1, 1, 0);
        let v200 = ResourceVersion::new(2, 0, 0);
        assert!(v100 < v110);
        assert!(v110 < v200);
        assert_eq!(v100, ResourceVersion::new(1, 0, 0));
    }

    /// Display format is "major.minor.patch".
    #[test]
    fn version_display() {
        assert_eq!(ResourceVersion::new(1, 2, 3).to_string(), "1.2.3");
    }

    // ── Channel display ──────────────────────────────────────────────

    /// Channel display strings match the expected lowercase names.
    #[test]
    fn channel_display() {
        assert_eq!(Channel::Dev.to_string(), "dev");
        assert_eq!(Channel::Beta.to_string(), "beta");
        assert_eq!(Channel::Release.to_string(), "release");
    }

    // ── ResourceCategory ─────────────────────────────────────────────

    /// Categories are lowercased and trimmed on construction.
    #[test]
    fn category_normalized() {
        let cat = ResourceCategory::new("  MAP  ");
        assert_eq!(cat.as_str(), "map");
    }
}
