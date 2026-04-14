// SPDX-License-Identifier: MIT OR Apache-2.0

//! Content-addressed blob identity and storage traits.
//!
//! Every Workshop package file is identified by its SHA-256 hash — the
//! `BlobId`. This is the foundation of content-addressed storage (CAS):
//! two files with identical content have the same `BlobId`, regardless of
//! their name, version, or publisher. Cross-version deduplication is
//! automatic and free.
//!
//! The `BlobStore` trait defines the interface for local CAS operations.
//! The initial implementation is filesystem-based:
//!
//! ```text
//! workshop/blobs/
//!   ab/cd/abcd1234…  ← first 4 hex chars as directory sharding
//! ```
//!
//! This layout mirrors git's object store (`objects/ab/cd1234…`), with
//! two-level sharding to avoid filesystem inode pressure on large stores.
//!
//! # Design authority
//!
//! - D049: content-addressed storage, deduplication across versions
//! - D030: SHA-256 as canonical hash algorithm

use std::fmt;

use sha2::{Digest, Sha256};

use crate::error::WorkshopError;

// ── Blob identity ────────────────────────────────────────────────────

/// SHA-256 content hash identifying a unique blob.
///
/// This is the canonical identity of Workshop content. Two blobs with
/// identical bytes produce the same `BlobId`. The 32-byte representation
/// is the raw hash; use `Display` for the hex-encoded form.
#[derive(Clone, Copy, PartialEq, Eq, Hash)]
pub struct BlobId([u8; 32]);

impl BlobId {
    /// Creates a `BlobId` from a raw 32-byte SHA-256 hash.
    pub const fn new(hash: [u8; 32]) -> Self {
        Self(hash)
    }

    /// Computes the `BlobId` (SHA-256) of the given data.
    pub fn from_data(data: &[u8]) -> Self {
        let mut hasher = Sha256::new();
        hasher.update(data);
        let result = hasher.finalize();
        let mut hash = [0u8; 32];
        hash.copy_from_slice(&result);
        Self(hash)
    }

    /// Returns the raw 32-byte hash.
    pub const fn as_bytes(&self) -> &[u8; 32] {
        &self.0
    }

    /// Returns the hex-encoded hash string (64 characters).
    pub fn to_hex(&self) -> String {
        self.0.iter().fold(String::with_capacity(64), |mut s, b| {
            use fmt::Write;
            let _ = write!(s, "{b:02x}");
            s
        })
    }

    /// Parses a `BlobId` from a 64-character hexadecimal string.
    ///
    /// Returns `None` if the input is not exactly 64 hex characters.
    /// Accepts both lowercase and uppercase hex digits.
    pub fn from_hex(hex: &str) -> Option<Self> {
        if hex.len() != 64 {
            return None;
        }
        let mut hash = [0u8; 32];
        for (i, slot) in hash.iter_mut().enumerate() {
            let pair = hex.get(i * 2..i * 2 + 2)?;
            *slot = u8::from_str_radix(pair, 16).ok()?;
        }
        Some(Self(hash))
    }

    /// Returns the two-level shard prefix for filesystem layout.
    ///
    /// For hash `abcd1234…`, returns `("ab", "cd")`. This distributes
    /// blobs across 65,536 directories, avoiding inode pressure.
    pub fn shard_prefix(&self) -> (String, String) {
        let hex = self.to_hex();
        // Safe: SHA-256 hex is always 64 ASCII chars, so indices 0..2 and 2..4
        // are always valid UTF-8-aligned positions.
        let first = hex.get(..2).unwrap_or("00");
        let second = hex.get(2..4).unwrap_or("00");
        (first.to_string(), second.to_string())
    }
}

impl fmt::Display for BlobId {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        for b in &self.0 {
            write!(f, "{b:02x}")?;
        }
        Ok(())
    }
}

impl fmt::Debug for BlobId {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        // Show truncated hash in debug output for readability.
        let hex = self.to_hex();
        let short = hex.get(..12).unwrap_or(&hex);
        write!(f, "BlobId({short}…)")
    }
}

// ── Blob store trait ─────────────────────────────────────────────────

/// Statistics from a garbage collection run.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct GcStats {
    /// Number of blobs removed (no longer referenced by any manifest).
    pub removed: u64,
    /// Number of bytes freed.
    pub bytes_freed: u64,
    /// Number of blobs retained.
    pub retained: u64,
}

/// Content-addressed storage interface.
///
/// Implementations store blobs by their SHA-256 hash and retrieve them
/// by `BlobId`. The store is append-mostly: blobs are written once and
/// read many times. Removal happens only during garbage collection.
///
/// # Implementations
///
/// - **Filesystem** (planned): sharded directory layout, `workshop/blobs/ab/cd/…`
/// - **In-memory** (for tests): `HashMap<BlobId, Vec<u8>>`
pub trait BlobStore {
    /// Stores data and returns its content hash.
    ///
    /// If the blob already exists, this is a no-op (idempotent).
    fn put(&mut self, data: &[u8]) -> Result<BlobId, WorkshopError>;

    /// Retrieves blob data by content hash.
    ///
    /// Returns `None` if the blob is not in the store.
    fn get(&self, id: &BlobId) -> Result<Option<Vec<u8>>, WorkshopError>;

    /// Checks whether a blob exists without reading its data.
    fn has(&self, id: &BlobId) -> Result<bool, WorkshopError>;

    /// Removes blobs not referenced by any known manifest.
    ///
    /// The caller provides the set of `BlobId`s that are still referenced.
    /// Everything else is removed.
    fn gc(&mut self, referenced: &[BlobId]) -> Result<GcStats, WorkshopError>;
}

// ── In-memory blob store (for tests) ─────────────────────────────────

/// In-memory CAS store for unit testing. Not for production use.
#[derive(Debug, Default)]
pub struct MemoryBlobStore {
    blobs: std::collections::HashMap<BlobId, Vec<u8>>,
}

impl BlobStore for MemoryBlobStore {
    fn put(&mut self, data: &[u8]) -> Result<BlobId, WorkshopError> {
        let id = BlobId::from_data(data);
        self.blobs.entry(id).or_insert_with(|| data.to_vec());
        Ok(id)
    }

    fn get(&self, id: &BlobId) -> Result<Option<Vec<u8>>, WorkshopError> {
        Ok(self.blobs.get(id).cloned())
    }

    fn has(&self, id: &BlobId) -> Result<bool, WorkshopError> {
        Ok(self.blobs.contains_key(id))
    }

    fn gc(&mut self, referenced: &[BlobId]) -> Result<GcStats, WorkshopError> {
        let before = self.blobs.len() as u64;
        let mut bytes_freed: u64 = 0;
        self.blobs.retain(|id, data| {
            if referenced.contains(id) {
                true
            } else {
                bytes_freed = bytes_freed.saturating_add(data.len() as u64);
                false
            }
        });
        let retained = self.blobs.len() as u64;
        let removed = before.saturating_sub(retained);
        Ok(GcStats {
            removed,
            bytes_freed,
            retained,
        })
    }
}

// ── Tests ────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;

    // ── BlobId construction ──────────────────────────────────────────

    /// BlobId from raw bytes preserves the hash exactly.
    #[test]
    fn blob_id_from_raw() {
        let hash = [0xAB; 32];
        let id = BlobId::new(hash);
        assert_eq!(id.as_bytes(), &hash);
    }

    /// BlobId::from_hex round-trips with to_hex correctly.
    ///
    /// Guarantees that hex encoding and decoding are inverse operations,
    /// so registry checksums can be converted to BlobIds losslessly.
    #[test]
    fn blob_id_from_hex_round_trip() {
        let original = BlobId::from_data(b"test content");
        let hex = original.to_hex();
        let parsed = BlobId::from_hex(&hex);
        assert_eq!(parsed, Some(original));
    }

    /// BlobId::from_hex rejects strings that are not 64 hex chars.
    #[test]
    fn blob_id_from_hex_wrong_length() {
        assert!(BlobId::from_hex("abcd").is_none());
        assert!(BlobId::from_hex("").is_none());
        assert!(BlobId::from_hex(&"a".repeat(63)).is_none());
        assert!(BlobId::from_hex(&"a".repeat(65)).is_none());
    }

    /// BlobId::from_hex rejects non-hex characters.
    #[test]
    fn blob_id_from_hex_invalid_chars() {
        let mut bad = "g".repeat(64);
        assert!(BlobId::from_hex(&bad).is_none());
        bad = "0".repeat(62) + "zz";
        assert!(BlobId::from_hex(&bad).is_none());
    }

    /// BlobId::from_data computes SHA-256 correctly.
    #[test]
    fn blob_id_from_data() {
        let data = b"hello workshop";
        let id = BlobId::from_data(data);
        // Verify it's 32 bytes (SHA-256 output size).
        assert_eq!(id.as_bytes().len(), 32);
        // Determinism: same input produces same hash.
        assert_eq!(id, BlobId::from_data(data));
    }

    /// Different data produces different BlobIds.
    #[test]
    fn different_data_different_ids() {
        let id1 = BlobId::from_data(b"content v1");
        let id2 = BlobId::from_data(b"content v2");
        assert_ne!(id1, id2);
    }

    // ── Display and hex encoding ─────────────────────────────────────

    /// Display produces a 64-character lowercase hex string.
    #[test]
    fn blob_id_display_hex() {
        let id = BlobId::new([0x00; 32]);
        let hex = id.to_string();
        assert_eq!(hex.len(), 64);
        assert!(hex.chars().all(|c| c.is_ascii_hexdigit()));
    }

    /// Debug shows a truncated hash for readability.
    #[test]
    fn blob_id_debug_truncated() {
        let id = BlobId::new([0xAB; 32]);
        let dbg = format!("{id:?}");
        assert!(dbg.starts_with("BlobId("), "{dbg}");
        assert!(dbg.contains("…"), "{dbg}");
    }

    // ── Shard prefix ─────────────────────────────────────────────────

    /// Shard prefix produces two-level directory components.
    #[test]
    fn shard_prefix_components() {
        let mut hash = [0u8; 32];
        hash[0] = 0xAB;
        hash[1] = 0xCD;
        let id = BlobId::new(hash);
        let (first, second) = id.shard_prefix();
        assert_eq!(first, "ab");
        assert_eq!(second, "cd");
    }

    // ── MemoryBlobStore ──────────────────────────────────────────────

    /// Put and get round-trips data correctly.
    #[test]
    fn memory_store_put_get() {
        let mut store = MemoryBlobStore::default();
        let data = b"test blob content";
        let id = store.put(data).unwrap();
        let retrieved = store.get(&id).unwrap().unwrap();
        assert_eq!(retrieved, data);
    }

    /// Put is idempotent — storing the same data twice is a no-op.
    #[test]
    fn memory_store_put_idempotent() {
        let mut store = MemoryBlobStore::default();
        let id1 = store.put(b"same content").unwrap();
        let id2 = store.put(b"same content").unwrap();
        assert_eq!(id1, id2);
    }

    /// Has returns true for stored blobs, false for unknown.
    #[test]
    fn memory_store_has() {
        let mut store = MemoryBlobStore::default();
        let id = store.put(b"exists").unwrap();
        let missing = BlobId::new([0xFF; 32]);
        assert!(store.has(&id).unwrap());
        assert!(!store.has(&missing).unwrap());
    }

    /// Get returns None for unknown blobs (not an error).
    #[test]
    fn memory_store_get_missing() {
        let store = MemoryBlobStore::default();
        let missing = BlobId::new([0xFF; 32]);
        assert!(store.get(&missing).unwrap().is_none());
    }

    /// GC removes unreferenced blobs and reports statistics.
    #[test]
    fn memory_store_gc() {
        let mut store = MemoryBlobStore::default();
        let keep_id = store.put(b"keep this").unwrap();
        let _remove_id = store.put(b"remove this").unwrap();

        let stats = store.gc(&[keep_id]).unwrap();
        assert_eq!(stats.removed, 1);
        assert_eq!(stats.retained, 1);
        assert!(stats.bytes_freed > 0);

        // Kept blob still accessible, removed blob gone.
        assert!(store.has(&keep_id).unwrap());
        assert!(store.get(&_remove_id).unwrap().is_none());
    }
}
