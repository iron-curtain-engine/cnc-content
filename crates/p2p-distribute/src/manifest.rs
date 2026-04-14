// SPDX-License-Identifier: MIT OR Apache-2.0

//! Group manifest for coordinated content replication across mirror nodes.
//!
//! A [`GroupManifest`] is a versioned, optionally-signed catalog listing every
//! file that belongs to a content group. The group master creates and signs
//! manifests; mirror nodes verify the signature and use [`diff_manifests`] to
//! compute what changed, then feed the diff into [`crate::catalog::plan_sync`]
//! to produce a download/delete plan.
//!
//! ## Design
//!
//! - **Signing is pluggable.** The manifest stores opaque `signature` and
//!   `signer_id` bytes. The crate does not depend on any cryptography library
//!   — callers provide their own Ed25519 / RSA / HMAC implementation and call
//!   [`GroupManifest::canonical_bytes`] to get the deterministic byte sequence
//!   to sign or verify.
//! - **Entries are sorted by path.** This ensures [`canonical_bytes`] is
//!   deterministic and enables O(n+m) merge-join diffing.
//! - **Paths are normalized.** Forward slashes only, no leading `/`, no `..`
//!   components. This prevents path traversal attacks from group masters.

use std::cmp::Ordering;

use thiserror::Error;

use crate::network_id::NetworkId;

// ── Errors ──────────────────────────────────────────────────────────

/// Errors from manifest construction or validation.
#[derive(Debug, Error)]
pub enum ManifestError {
    /// Content entry path is empty.
    #[error("content entry path must not be empty")]
    EmptyPath,

    /// Content entry path contains invalid characters or components.
    #[error("invalid path `{path}`: {reason}")]
    InvalidPath {
        /// The offending path.
        path: String,
        /// Why the path is invalid.
        reason: &'static str,
    },

    /// Two entries share the same path.
    #[error("duplicate path `{path}` in manifest")]
    DuplicatePath {
        /// The duplicated path.
        path: String,
    },

    /// Manifest version must be greater than zero.
    #[error("manifest version must be > 0 (version 0 is reserved)")]
    ZeroVersion,
}

// ── Path validation ─────────────────────────────────────────────────

/// Validates a content entry path.
///
/// Valid paths use forward slashes, contain no `..` components, no leading
/// `/`, no backslashes, and no null bytes. This matches the path rules
/// enforced by `strict-path` for archive extraction.
fn validate_path(path: &str) -> Result<(), ManifestError> {
    if path.is_empty() {
        return Err(ManifestError::EmptyPath);
    }
    if path.contains('\\') {
        return Err(ManifestError::InvalidPath {
            path: path.to_owned(),
            reason: "backslashes are not allowed (use forward slashes)",
        });
    }
    if path.starts_with('/') {
        return Err(ManifestError::InvalidPath {
            path: path.to_owned(),
            reason: "absolute paths are not allowed (no leading slash)",
        });
    }
    if path.contains("..") {
        return Err(ManifestError::InvalidPath {
            path: path.to_owned(),
            reason: "parent traversal (`..`) is not allowed",
        });
    }
    if path.as_bytes().contains(&0) {
        return Err(ManifestError::InvalidPath {
            path: path.to_owned(),
            reason: "null bytes are not allowed",
        });
    }
    Ok(())
}

// ── ContentEntry ────────────────────────────────────────────────────

/// A single file entry in a group manifest.
///
/// Identifies a file by its relative path, SHA-256 content hash, and size.
/// An optional torrent `info_hash` links the entry to BitTorrent metadata
/// for piece-level downloading.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ContentEntry {
    /// Relative path within the content directory (forward slashes, normalized).
    path: String,
    /// SHA-256 hash of the complete file contents.
    content_hash: [u8; 32],
    /// File size in bytes.
    file_size: u64,
    /// Optional SHA-1 info hash of the torrent metadata for this file.
    /// When present, mirrors can download the file via BitTorrent/web seeds.
    info_hash: Option<[u8; 20]>,
}

impl ContentEntry {
    /// Creates a new content entry with path validation.
    ///
    /// Paths must use forward slashes, contain no `..` components, and not
    /// start with `/`. See [`validate_path`] for the full rule set.
    pub fn new(
        path: impl Into<String>,
        content_hash: [u8; 32],
        file_size: u64,
    ) -> Result<Self, ManifestError> {
        let path = path.into();
        validate_path(&path)?;
        Ok(Self {
            path,
            content_hash,
            file_size,
            info_hash: None,
        })
    }

    /// Attaches a torrent info hash to this entry.
    ///
    /// The info hash is the SHA-1 of the torrent's info dictionary, used to
    /// look up torrent metadata for piece-level downloading.
    pub fn with_info_hash(mut self, info_hash: [u8; 20]) -> Self {
        self.info_hash = Some(info_hash);
        self
    }

    /// Returns the relative path of this entry.
    pub fn path(&self) -> &str {
        &self.path
    }

    /// Returns the SHA-256 content hash.
    pub fn content_hash(&self) -> &[u8; 32] {
        &self.content_hash
    }

    /// Returns the file size in bytes.
    pub fn file_size(&self) -> u64 {
        self.file_size
    }

    /// Returns the optional torrent info hash.
    pub fn info_hash(&self) -> Option<&[u8; 20]> {
        self.info_hash.as_ref()
    }
}

// ── GroupManifest ───────────────────────────────────────────────────

/// A versioned, optionally-signed content catalog for a group of mirror nodes.
///
/// The group master creates manifests listing every file in the group. Mirror
/// nodes download the manifest, verify the signature, diff against their local
/// state, and download/delete files to stay in sync.
///
/// ## Invariants
///
/// - Entries are sorted by path (enforced by [`ManifestBuilder::build`]).
/// - No two entries share the same path.
/// - Version is always > 0 (version 0 is reserved as "no manifest").
#[derive(Debug, Clone)]
pub struct GroupManifest {
    /// Monotonically increasing version number.
    version: u64,
    /// Network identity scoping this manifest to a specific group.
    network_id: NetworkId,
    /// Unix timestamp (seconds since epoch) when this manifest was created.
    created_at: u64,
    /// Content entries, sorted by path.
    entries: Vec<ContentEntry>,
    /// Opaque signature over [`canonical_bytes`], produced by the caller's
    /// signing implementation. Empty if unsigned.
    signature: Vec<u8>,
    /// Opaque identifier of the signer (e.g. Ed25519 public key bytes).
    /// Empty if unsigned.
    signer_id: Vec<u8>,
}

impl GroupManifest {
    /// Creates a new [`ManifestBuilder`] for the given group.
    pub fn builder(network_id: NetworkId) -> ManifestBuilder {
        ManifestBuilder {
            network_id,
            version: 0,
            created_at: 0,
            entries: Vec::new(),
        }
    }

    /// Returns the manifest version number (always > 0).
    pub fn version(&self) -> u64 {
        self.version
    }

    /// Returns the group's network identity.
    pub fn network_id(&self) -> &NetworkId {
        &self.network_id
    }

    /// Returns the creation timestamp (unix seconds).
    pub fn created_at(&self) -> u64 {
        self.created_at
    }

    /// Returns the content entries, sorted by path.
    pub fn entries(&self) -> &[ContentEntry] {
        &self.entries
    }

    /// Returns the number of content entries.
    pub fn entry_count(&self) -> usize {
        self.entries.len()
    }

    /// Looks up an entry by path using binary search.
    ///
    /// Returns `None` if no entry with the given path exists.
    pub fn get_entry(&self, path: &str) -> Option<&ContentEntry> {
        self.entries
            .binary_search_by(|e| e.path.as_str().cmp(path))
            .ok()
            .and_then(|i| self.entries.get(i))
    }

    /// Returns `true` if this manifest has been signed.
    pub fn is_signed(&self) -> bool {
        !self.signature.is_empty()
    }

    /// Returns the opaque signature bytes (empty if unsigned).
    pub fn signature(&self) -> &[u8] {
        &self.signature
    }

    /// Returns the opaque signer identifier (empty if unsigned).
    pub fn signer_id(&self) -> &[u8] {
        &self.signer_id
    }

    /// Attaches a cryptographic signature and signer identity.
    ///
    /// The caller is responsible for computing the signature over
    /// [`canonical_bytes`](Self::canonical_bytes) using their chosen crypto
    /// library (Ed25519, RSA, HMAC, etc.).
    pub fn set_signature(&mut self, signature: Vec<u8>, signer_id: Vec<u8>) {
        self.signature = signature;
        self.signer_id = signer_id;
    }

    /// Returns the deterministic byte representation for signing/verification.
    ///
    /// The encoding is:
    /// - Version (u64 LE)
    /// - NetworkId (32 bytes)
    /// - Created-at timestamp (u64 LE)
    /// - Entry count (u32 LE)
    /// - For each entry (sorted by path):
    ///   - Path length (u32 LE) + path bytes (UTF-8)
    ///   - Content hash (32 bytes)
    ///   - File size (u64 LE)
    ///   - Info hash flag (1 byte: 0=None, 1=Some) + optional 20 bytes
    ///
    /// The same manifest always produces the same bytes. The signature and
    /// signer_id are NOT included (they are bound to the canonical bytes).
    pub fn canonical_bytes(&self) -> Vec<u8> {
        // Estimate capacity: 52 bytes header + ~80 bytes per entry.
        let estimated = 52_usize.saturating_add(self.entries.len().saturating_mul(80));
        let mut buf = Vec::with_capacity(estimated);

        buf.extend_from_slice(&self.version.to_le_bytes());
        buf.extend_from_slice(self.network_id.as_bytes());
        buf.extend_from_slice(&self.created_at.to_le_bytes());

        let count = self.entries.len() as u32;
        buf.extend_from_slice(&count.to_le_bytes());

        for entry in &self.entries {
            let path_bytes = entry.path.as_bytes();
            buf.extend_from_slice(&(path_bytes.len() as u32).to_le_bytes());
            buf.extend_from_slice(path_bytes);
            buf.extend_from_slice(&entry.content_hash);
            buf.extend_from_slice(&entry.file_size.to_le_bytes());
            match &entry.info_hash {
                Some(h) => {
                    buf.push(1);
                    buf.extend_from_slice(h);
                }
                None => {
                    buf.push(0);
                }
            }
        }

        buf
    }

    /// Returns the total size in bytes across all entries.
    pub fn total_size(&self) -> u64 {
        self.entries
            .iter()
            .map(|e| e.file_size)
            .fold(0u64, u64::saturating_add)
    }
}

// ── ManifestBuilder ─────────────────────────────────────────────────

/// Builder for constructing a [`GroupManifest`] with validated entries.
///
/// Entries are automatically sorted by path on [`build`](Self::build).
/// Duplicate paths are rejected.
pub struct ManifestBuilder {
    network_id: NetworkId,
    version: u64,
    created_at: u64,
    entries: Vec<ContentEntry>,
}

impl ManifestBuilder {
    /// Sets the manifest version (must be > 0).
    pub fn version(mut self, v: u64) -> Self {
        self.version = v;
        self
    }

    /// Sets the creation timestamp (unix seconds since epoch).
    pub fn created_at(mut self, ts: u64) -> Self {
        self.created_at = ts;
        self
    }

    /// Adds a content entry to the manifest.
    pub fn add_entry(mut self, entry: ContentEntry) -> Self {
        self.entries.push(entry);
        self
    }

    /// Builds the manifest, sorting entries and validating invariants.
    ///
    /// Returns [`ManifestError::ZeroVersion`] if version is 0, or
    /// [`ManifestError::DuplicatePath`] if two entries share a path.
    pub fn build(mut self) -> Result<GroupManifest, ManifestError> {
        if self.version == 0 {
            return Err(ManifestError::ZeroVersion);
        }

        // Sort entries by path for deterministic canonical_bytes and O(log n)
        // binary search in get_entry.
        self.entries.sort_by(|a, b| a.path.cmp(&b.path));

        // Reject duplicate paths (adjacent after sort).
        for pair in self.entries.windows(2) {
            if let (Some(a), Some(b)) = (pair.first(), pair.last()) {
                if a.path == b.path {
                    return Err(ManifestError::DuplicatePath {
                        path: a.path.clone(),
                    });
                }
            }
        }

        Ok(GroupManifest {
            version: self.version,
            network_id: self.network_id,
            created_at: self.created_at,
            entries: self.entries,
            signature: Vec::new(),
            signer_id: Vec::new(),
        })
    }
}

// ── Diffing ─────────────────────────────────────────────────────────

/// A difference between two manifests.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ManifestDiff {
    /// A file was added in the new manifest.
    Added {
        /// The new entry.
        entry: ContentEntry,
    },
    /// A file was removed from the new manifest.
    Removed {
        /// Path of the removed file.
        path: String,
    },
    /// A file's content changed between manifests.
    Modified {
        /// Path of the modified file.
        path: String,
        /// SHA-256 hash from the old manifest.
        old_hash: [u8; 32],
        /// The updated entry from the new manifest.
        new_entry: ContentEntry,
    },
}

/// Computes the differences between two sorted entry slices.
///
/// Both slices **must** be sorted by path (guaranteed by [`GroupManifest`]).
/// Uses an O(n+m) merge-join: walks both slices simultaneously, comparing
/// paths lexicographically.
///
/// Typical usage:
/// ```
/// # use p2p_distribute::manifest::{ContentEntry, GroupManifest, diff_manifests};
/// # use p2p_distribute::NetworkId;
/// let old = GroupManifest::builder(NetworkId::TEST)
///     .version(1)
///     .add_entry(ContentEntry::new("a.txt", [0xAA; 32], 100).unwrap())
///     .build().unwrap();
/// let new = GroupManifest::builder(NetworkId::TEST)
///     .version(2)
///     .add_entry(ContentEntry::new("a.txt", [0xBB; 32], 200).unwrap())
///     .add_entry(ContentEntry::new("b.txt", [0xCC; 32], 300).unwrap())
///     .build().unwrap();
/// let diffs = diff_manifests(old.entries(), new.entries());
/// assert_eq!(diffs.len(), 2); // a.txt modified, b.txt added
/// ```
pub fn diff_manifests(old: &[ContentEntry], new: &[ContentEntry]) -> Vec<ManifestDiff> {
    let mut result = Vec::new();
    let mut old_iter = old.iter().peekable();
    let mut new_iter = new.iter().peekable();

    loop {
        match (old_iter.peek(), new_iter.peek()) {
            (None, None) => break,

            // Remaining old entries were removed.
            (Some(old_entry), None) => {
                result.push(ManifestDiff::Removed {
                    path: old_entry.path.clone(),
                });
                old_iter.next();
            }

            // Remaining new entries were added.
            (None, Some(new_entry)) => {
                result.push(ManifestDiff::Added {
                    entry: (*new_entry).clone(),
                });
                new_iter.next();
            }

            (Some(old_entry), Some(new_entry)) => {
                match old_entry.path.cmp(&new_entry.path) {
                    // Old path comes first → it was removed.
                    Ordering::Less => {
                        result.push(ManifestDiff::Removed {
                            path: old_entry.path.clone(),
                        });
                        old_iter.next();
                    }
                    // New path comes first → it was added.
                    Ordering::Greater => {
                        result.push(ManifestDiff::Added {
                            entry: (*new_entry).clone(),
                        });
                        new_iter.next();
                    }
                    // Same path — check if content changed.
                    Ordering::Equal => {
                        if old_entry.content_hash != new_entry.content_hash
                            || old_entry.file_size != new_entry.file_size
                        {
                            result.push(ManifestDiff::Modified {
                                path: old_entry.path.clone(),
                                old_hash: old_entry.content_hash,
                                new_entry: (*new_entry).clone(),
                            });
                        }
                        old_iter.next();
                        new_iter.next();
                    }
                }
            }
        }
    }

    result
}

// ── Tests ───────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;
    use crate::network_id::NetworkId;

    /// Helper: creates a content entry with a fill-byte hash.
    fn entry(path: &str, hash_byte: u8, size: u64) -> ContentEntry {
        ContentEntry::new(path, [hash_byte; 32], size).unwrap()
    }

    // ── Path validation ─────────────────────────────────────────────

    /// Empty paths are rejected.
    ///
    /// An empty path has no meaning as a file location and would cause
    /// ambiguous behaviour during extraction.
    #[test]
    fn path_rejects_empty() {
        let err = ContentEntry::new("", [0; 32], 0).unwrap_err();
        assert!(err.to_string().contains("empty"));
    }

    /// Backslashes are rejected — paths must use forward slashes.
    ///
    /// Normalizing to forward slashes prevents platform-dependent path
    /// behaviour and ensures canonical_bytes is deterministic.
    #[test]
    fn path_rejects_backslash() {
        let err = ContentEntry::new("dir\\file.txt", [0; 32], 0).unwrap_err();
        assert!(err.to_string().contains("backslash"));
    }

    /// Leading slashes are rejected — paths must be relative.
    ///
    /// Absolute paths in a manifest could escape the content directory.
    #[test]
    fn path_rejects_leading_slash() {
        let err = ContentEntry::new("/etc/passwd", [0; 32], 0).unwrap_err();
        assert!(err.to_string().contains("absolute"));
    }

    /// Parent traversal (`..`) is rejected.
    ///
    /// Prevents Zip-Slip-style path traversal attacks from a malicious
    /// group master.
    #[test]
    fn path_rejects_parent_traversal() {
        let err = ContentEntry::new("../secret.txt", [0; 32], 0).unwrap_err();
        assert!(err.to_string().contains("traversal"));
    }

    /// Null bytes are rejected.
    ///
    /// Null bytes in paths can truncate filenames on some platforms,
    /// leading to misdirected writes.
    #[test]
    fn path_rejects_null_bytes() {
        let err = ContentEntry::new("file\0.txt", [0; 32], 0).unwrap_err();
        assert!(err.to_string().contains("null"));
    }

    /// Valid nested paths are accepted.
    #[test]
    fn path_accepts_nested_forward_slashes() {
        let e = ContentEntry::new("maps/subfolder/map1.mix", [0xAA; 32], 1024).unwrap();
        assert_eq!(e.path(), "maps/subfolder/map1.mix");
    }

    // ── ContentEntry ────────────────────────────────────────────────

    /// Fields round-trip through construction and getters.
    #[test]
    fn content_entry_roundtrip() {
        let hash = [0x42; 32];
        let e = ContentEntry::new("data/file.bin", hash, 99_999).unwrap();
        assert_eq!(e.path(), "data/file.bin");
        assert_eq!(e.content_hash(), &hash);
        assert_eq!(e.file_size(), 99_999);
        assert_eq!(e.info_hash(), None);
    }

    /// `with_info_hash` attaches a torrent info hash.
    #[test]
    fn content_entry_with_info_hash() {
        let ih = [0xBE; 20];
        let e = ContentEntry::new("file.zip", [0; 32], 500)
            .unwrap()
            .with_info_hash(ih);
        assert_eq!(e.info_hash(), Some(&ih));
    }

    // ── ManifestBuilder ─────────────────────────────────────────────

    /// Builder rejects version 0.
    ///
    /// Version 0 is reserved as "no manifest" so mirrors can distinguish
    /// "never synced" from "synced to version 1".
    #[test]
    fn builder_rejects_zero_version() {
        let result = GroupManifest::builder(NetworkId::TEST).version(0).build();
        assert!(result.is_err());
        assert!(result.unwrap_err().to_string().contains("version"));
    }

    /// Builder sorts entries by path.
    ///
    /// Sorted entries are required for deterministic canonical_bytes and
    /// O(log n) binary search in get_entry.
    #[test]
    fn builder_sorts_entries() {
        let m = GroupManifest::builder(NetworkId::TEST)
            .version(1)
            .add_entry(entry("z.txt", 0, 1))
            .add_entry(entry("a.txt", 0, 2))
            .add_entry(entry("m.txt", 0, 3))
            .build()
            .unwrap();
        let paths: Vec<&str> = m.entries().iter().map(|e| e.path()).collect();
        assert_eq!(paths, vec!["a.txt", "m.txt", "z.txt"]);
    }

    /// Builder rejects duplicate paths.
    ///
    /// Duplicate paths would cause ambiguous behaviour — which version of
    /// the file should the mirror keep?
    #[test]
    fn builder_rejects_duplicates() {
        let result = GroupManifest::builder(NetworkId::TEST)
            .version(1)
            .add_entry(entry("same.txt", 0xAA, 1))
            .add_entry(entry("same.txt", 0xBB, 2))
            .build();
        assert!(result.is_err());
        assert!(result.unwrap_err().to_string().contains("same.txt"));
    }

    /// Empty manifest (no entries) is valid.
    ///
    /// An empty manifest means "delete everything" — the group has no
    /// content. This is a valid state (e.g. group decommissioning).
    #[test]
    fn builder_allows_empty_entries() {
        let m = GroupManifest::builder(NetworkId::TEST)
            .version(1)
            .build()
            .unwrap();
        assert_eq!(m.entry_count(), 0);
    }

    // ── GroupManifest ───────────────────────────────────────────────

    /// `get_entry` finds entries by path via binary search.
    #[test]
    fn get_entry_finds_by_path() {
        let m = GroupManifest::builder(NetworkId::TEST)
            .version(1)
            .add_entry(entry("alpha.bin", 0xAA, 100))
            .add_entry(entry("beta.bin", 0xBB, 200))
            .build()
            .unwrap();

        let found = m.get_entry("beta.bin").unwrap();
        assert_eq!(found.file_size(), 200);
        assert!(m.get_entry("gamma.bin").is_none());
    }

    /// `total_size` sums all entry sizes.
    #[test]
    fn total_size_sums_entries() {
        let m = GroupManifest::builder(NetworkId::TEST)
            .version(1)
            .add_entry(entry("a", 0, 100))
            .add_entry(entry("b", 0, 200))
            .add_entry(entry("c", 0, 300))
            .build()
            .unwrap();
        assert_eq!(m.total_size(), 600);
    }

    // ── Signing ─────────────────────────────────────────────────────

    /// Unsigned manifest reports `is_signed() == false`.
    #[test]
    fn unsigned_manifest() {
        let m = GroupManifest::builder(NetworkId::TEST)
            .version(1)
            .build()
            .unwrap();
        assert!(!m.is_signed());
        assert!(m.signature().is_empty());
        assert!(m.signer_id().is_empty());
    }

    /// `set_signature` attaches signature and signer_id.
    #[test]
    fn set_signature_marks_as_signed() {
        let mut m = GroupManifest::builder(NetworkId::TEST)
            .version(1)
            .build()
            .unwrap();
        m.set_signature(vec![0xDE, 0xAD], vec![0xBE, 0xEF]);
        assert!(m.is_signed());
        assert_eq!(m.signature(), &[0xDE, 0xAD]);
        assert_eq!(m.signer_id(), &[0xBE, 0xEF]);
    }

    // ── Canonical bytes ─────────────────────────────────────────────

    /// Same manifest always produces the same canonical bytes.
    ///
    /// Determinism is critical — signing the same content must produce
    /// the same signature, and verification must compare identical bytes.
    #[test]
    fn canonical_bytes_deterministic() {
        let m = GroupManifest::builder(NetworkId::TEST)
            .version(1)
            .created_at(1_700_000_000)
            .add_entry(entry("file.bin", 0xAA, 1024))
            .build()
            .unwrap();
        let b1 = m.canonical_bytes();
        let b2 = m.canonical_bytes();
        assert_eq!(b1, b2);
    }

    /// Changing the version changes canonical bytes.
    ///
    /// This ensures version bumps invalidate old signatures.
    #[test]
    fn canonical_bytes_differ_on_version_change() {
        let m1 = GroupManifest::builder(NetworkId::TEST)
            .version(1)
            .add_entry(entry("f.bin", 0, 10))
            .build()
            .unwrap();
        let m2 = GroupManifest::builder(NetworkId::TEST)
            .version(2)
            .add_entry(entry("f.bin", 0, 10))
            .build()
            .unwrap();
        assert_ne!(m1.canonical_bytes(), m2.canonical_bytes());
    }

    /// Canonical bytes include the info_hash when present.
    ///
    /// Entries with and without info_hash must produce different bytes,
    /// so a changed info_hash invalidates the signature.
    #[test]
    fn canonical_bytes_differ_with_info_hash() {
        let e_no_ih = ContentEntry::new("f.bin", [0; 32], 10).unwrap();
        let e_with_ih = ContentEntry::new("f.bin", [0; 32], 10)
            .unwrap()
            .with_info_hash([0xFF; 20]);

        let m1 = GroupManifest::builder(NetworkId::TEST)
            .version(1)
            .add_entry(e_no_ih)
            .build()
            .unwrap();
        let m2 = GroupManifest::builder(NetworkId::TEST)
            .version(1)
            .add_entry(e_with_ih)
            .build()
            .unwrap();
        assert_ne!(m1.canonical_bytes(), m2.canonical_bytes());
    }

    // ── Diffing ─────────────────────────────────────────────────────

    /// Identical manifests produce an empty diff.
    #[test]
    fn diff_no_changes() {
        let entries = &[entry("a.txt", 0xAA, 100), entry("b.txt", 0xBB, 200)];
        let diffs = diff_manifests(entries, entries);
        assert!(diffs.is_empty());
    }

    /// Files only in the new manifest appear as Added.
    #[test]
    fn diff_added_files() {
        let old: &[ContentEntry] = &[];
        let new = &[entry("new_file.bin", 0xCC, 500)];
        let diffs = diff_manifests(old, new);
        assert_eq!(diffs.len(), 1);
        assert!(
            matches!(&diffs[0], ManifestDiff::Added { entry } if entry.path() == "new_file.bin")
        );
    }

    /// Files only in the old manifest appear as Removed.
    #[test]
    fn diff_removed_files() {
        let old = &[entry("gone.bin", 0xDD, 300)];
        let new: &[ContentEntry] = &[];
        let diffs = diff_manifests(old, new);
        assert_eq!(diffs.len(), 1);
        assert!(matches!(&diffs[0], ManifestDiff::Removed { path } if path == "gone.bin"));
    }

    /// Files with the same path but different hash appear as Modified.
    #[test]
    fn diff_modified_files() {
        let old = &[entry("data.bin", 0xAA, 100)];
        let new = &[entry("data.bin", 0xBB, 200)];
        let diffs = diff_manifests(old, new);
        assert_eq!(diffs.len(), 1);
        match &diffs[0] {
            ManifestDiff::Modified {
                path,
                old_hash,
                new_entry,
            } => {
                assert_eq!(path, "data.bin");
                assert_eq!(old_hash, &[0xAA; 32]);
                assert_eq!(new_entry.file_size(), 200);
            }
            other => panic!("expected Modified, got {other:?}"),
        }
    }

    /// Mixed adds, removes, and modifications in a single diff.
    #[test]
    fn diff_mixed_changes() {
        let old = &[
            entry("a.txt", 0xAA, 100), // unchanged
            entry("b.txt", 0xBB, 200), // will be modified
            entry("c.txt", 0xCC, 300), // will be removed
        ];
        let new = &[
            entry("a.txt", 0xAA, 100), // unchanged
            entry("b.txt", 0xFF, 999), // modified (different hash + size)
            entry("d.txt", 0xDD, 400), // added
        ];
        let diffs = diff_manifests(old, new);
        assert_eq!(diffs.len(), 3);
        assert!(matches!(&diffs[0], ManifestDiff::Modified { path, .. } if path == "b.txt"));
        assert!(matches!(&diffs[1], ManifestDiff::Removed { path } if path == "c.txt"));
        assert!(matches!(&diffs[2], ManifestDiff::Added { entry } if entry.path() == "d.txt"));
    }

    /// Both slices empty produces an empty diff.
    #[test]
    fn diff_both_empty() {
        let diffs = diff_manifests(&[], &[]);
        assert!(diffs.is_empty());
    }

    // ── Error Display ───────────────────────────────────────────────

    /// ManifestError::InvalidPath display includes the path and reason.
    ///
    /// Ensures error messages carry enough context for diagnostics.
    #[test]
    fn error_display_includes_context() {
        let err = ManifestError::InvalidPath {
            path: "bad\\path".to_owned(),
            reason: "backslashes are not allowed (use forward slashes)",
        };
        let msg = err.to_string();
        assert!(msg.contains("bad\\path"));
        assert!(msg.contains("backslash"));
    }
}
