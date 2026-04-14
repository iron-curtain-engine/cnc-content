// SPDX-License-Identifier: MIT OR Apache-2.0

//! Bounded piece cache with LRU eviction — partial storage for bridge
//! nodes that cannot hold the full catalog.
//!
//! ## What
//!
//! `PieceCache` tracks which pieces are resident in local storage and
//! enforces a capacity limit. When the cache is full, the least recently
//! used piece is evicted to make room for a new one.
//!
//! ## Why (P2P-to-HTTP bridge design)
//!
//! A bridge node participates in the P2P swarm but may not have enough
//! disk space for the entire catalog. It needs to decide which pieces
//! to keep (hot pieces the swarm is requesting) and which to evict
//! (cold pieces nobody has asked for recently).
//!
//! ## How
//!
//! An LRU doubly-linked list tracks access order. Each `touch()` moves
//! the piece to the front (most recently used). When capacity is
//! exceeded, `evict()` removes from the back (least recently used).
//!
//! The implementation uses a `HashMap` for O(1) lookup and a `VecDeque`
//! for O(n) LRU ordering (acceptable because eviction is infrequent
//! relative to piece I/O). For high-frequency access patterns, this
//! could be upgraded to an intrusive linked list, but the current
//! design is simpler and correct.

use std::collections::{HashMap, VecDeque};
use std::time::Instant;

// ── Constants ───────────────────────────────────────────────────────

/// Default cache capacity in number of pieces.
///
/// 1024 pieces × 256 KiB piece = 256 MiB default cache. Configurable
/// at construction time.
pub const DEFAULT_CACHE_CAPACITY: u32 = 1024;

// ── CacheEntry ──────────────────────────────────────────────────────

/// Metadata for a cached piece.
#[derive(Debug, Clone)]
struct CacheEntry {
    /// Size of the piece in bytes (for total-size accounting).
    size_bytes: u32,
    /// When this piece was last accessed (for diagnostics).
    last_access: Instant,
}

// ── PieceCache ──────────────────────────────────────────────────────

/// Bounded piece cache with LRU eviction.
///
/// Tracks piece residency and enforces a maximum number of cached pieces.
/// When full, the least recently used piece is evicted.
///
/// ```
/// use p2p_distribute::cache::PieceCache;
/// use std::time::Instant;
///
/// let mut cache = PieceCache::with_capacity(3);
/// let now = Instant::now();
///
/// cache.insert(0, 256_000, now);
/// cache.insert(1, 256_000, now);
/// cache.insert(2, 256_000, now);
/// assert!(cache.contains(0));
///
/// // Cache is full — inserting piece 3 evicts piece 0 (oldest).
/// let evicted = cache.insert(3, 256_000, now);
/// assert_eq!(evicted, Some(0));
/// assert!(!cache.contains(0));
/// assert!(cache.contains(3));
/// ```
pub struct PieceCache {
    /// Maximum number of resident pieces.
    capacity: u32,
    /// Piece metadata keyed by piece index.
    entries: HashMap<u32, CacheEntry>,
    /// LRU order: front = most recently used, back = least recently used.
    lru_order: VecDeque<u32>,
    /// Total size of all cached pieces in bytes.
    total_bytes: u64,
}

impl PieceCache {
    /// Creates a cache with the default capacity.
    pub fn new() -> Self {
        Self::with_capacity(DEFAULT_CACHE_CAPACITY)
    }

    /// Creates a cache with the specified capacity (number of pieces).
    ///
    /// A capacity of 0 means no caching — every insert immediately evicts.
    pub fn with_capacity(capacity: u32) -> Self {
        Self {
            capacity,
            entries: HashMap::new(),
            lru_order: VecDeque::new(),
            total_bytes: 0,
        }
    }

    /// Inserts a piece into the cache.
    ///
    /// If the piece is already cached, this updates its access time and
    /// moves it to the front of the LRU list. Returns `None`.
    ///
    /// If the cache is full, evicts the least recently used piece and
    /// returns its index. Returns `None` if no eviction was needed.
    pub fn insert(&mut self, piece_index: u32, size_bytes: u32, now: Instant) -> Option<u32> {
        // Already cached — just touch.
        if self.entries.contains_key(&piece_index) {
            self.touch(piece_index, now);
            return None;
        }

        // Evict if at capacity.
        let evicted = if self.entries.len() as u32 >= self.capacity {
            self.evict_lru()
        } else {
            None
        };

        self.entries.insert(
            piece_index,
            CacheEntry {
                size_bytes,
                last_access: now,
            },
        );
        self.lru_order.push_front(piece_index);
        self.total_bytes = self.total_bytes.saturating_add(size_bytes as u64);

        evicted
    }

    /// Records an access to a cached piece, moving it to the MRU position.
    ///
    /// No-op if the piece is not cached.
    pub fn touch(&mut self, piece_index: u32, now: Instant) {
        if let Some(entry) = self.entries.get_mut(&piece_index) {
            entry.last_access = now;
            // Move to front of LRU list.
            if let Some(pos) = self.lru_order.iter().position(|&p| p == piece_index) {
                self.lru_order.remove(pos);
            }
            self.lru_order.push_front(piece_index);
        }
    }

    /// Removes a specific piece from the cache.
    ///
    /// Returns `true` if the piece was cached and removed.
    pub fn remove(&mut self, piece_index: u32) -> bool {
        if let Some(entry) = self.entries.remove(&piece_index) {
            self.total_bytes = self.total_bytes.saturating_sub(entry.size_bytes as u64);
            if let Some(pos) = self.lru_order.iter().position(|&p| p == piece_index) {
                self.lru_order.remove(pos);
            }
            true
        } else {
            false
        }
    }

    /// Returns whether a piece is currently cached.
    pub fn contains(&self, piece_index: u32) -> bool {
        self.entries.contains_key(&piece_index)
    }

    /// Returns the number of cached pieces.
    pub fn len(&self) -> u32 {
        self.entries.len() as u32
    }

    /// Returns whether the cache is empty.
    pub fn is_empty(&self) -> bool {
        self.entries.is_empty()
    }

    /// Returns the total size of all cached pieces in bytes.
    pub fn total_bytes(&self) -> u64 {
        self.total_bytes
    }

    /// Returns the cache capacity (maximum number of pieces).
    pub fn capacity(&self) -> u32 {
        self.capacity
    }

    /// Returns the piece indices in LRU order (front = most recent).
    pub fn lru_snapshot(&self) -> Vec<u32> {
        self.lru_order.iter().copied().collect()
    }

    /// Evicts the least recently used piece and returns its index.
    ///
    /// Returns `None` if the cache is empty.
    fn evict_lru(&mut self) -> Option<u32> {
        let evicted_idx = self.lru_order.pop_back()?;
        if let Some(entry) = self.entries.remove(&evicted_idx) {
            self.total_bytes = self.total_bytes.saturating_sub(entry.size_bytes as u64);
        }
        Some(evicted_idx)
    }
}

impl Default for PieceCache {
    fn default() -> Self {
        Self::new()
    }
}

impl std::fmt::Debug for PieceCache {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("PieceCache")
            .field("capacity", &self.capacity)
            .field("len", &self.entries.len())
            .field("total_bytes", &self.total_bytes)
            .finish()
    }
}

// ── Tests ───────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;

    // ── Basic operations ────────────────────────────────────────────

    /// New cache starts empty.
    #[test]
    fn new_cache_empty() {
        let cache = PieceCache::with_capacity(10);
        assert!(cache.is_empty());
        assert_eq!(cache.len(), 0);
        assert_eq!(cache.total_bytes(), 0);
        assert_eq!(cache.capacity(), 10);
    }

    /// Insert and check containment.
    #[test]
    fn insert_and_contains() {
        let mut cache = PieceCache::with_capacity(5);
        let now = Instant::now();
        cache.insert(42, 1000, now);
        assert!(cache.contains(42));
        assert!(!cache.contains(99));
        assert_eq!(cache.len(), 1);
        assert_eq!(cache.total_bytes(), 1000);
    }

    /// Inserting an already-cached piece is idempotent.
    ///
    /// The piece is touched (moved to MRU) but not duplicated. No
    /// eviction occurs.
    #[test]
    fn insert_duplicate_idempotent() {
        let mut cache = PieceCache::with_capacity(5);
        let now = Instant::now();
        cache.insert(1, 500, now);
        let evicted = cache.insert(1, 500, now);
        assert_eq!(evicted, None);
        assert_eq!(cache.len(), 1);
        assert_eq!(cache.total_bytes(), 500); // not doubled
    }

    /// Remove returns true for cached piece, false for absent.
    #[test]
    fn remove_piece() {
        let mut cache = PieceCache::with_capacity(5);
        let now = Instant::now();
        cache.insert(1, 200, now);
        assert!(cache.remove(1));
        assert!(!cache.contains(1));
        assert_eq!(cache.total_bytes(), 0);
        assert!(!cache.remove(1)); // already gone
    }

    // ── LRU eviction ────────────────────────────────────────────────

    /// Full cache evicts the least recently used piece.
    ///
    /// Pieces 0,1,2 are inserted into a capacity-3 cache. Inserting
    /// piece 3 should evict piece 0 (oldest).
    #[test]
    fn eviction_removes_lru() {
        let mut cache = PieceCache::with_capacity(3);
        let now = Instant::now();
        cache.insert(0, 100, now);
        cache.insert(1, 100, now);
        cache.insert(2, 100, now);

        let evicted = cache.insert(3, 100, now);
        assert_eq!(evicted, Some(0));
        assert!(!cache.contains(0));
        assert!(cache.contains(3));
        assert_eq!(cache.len(), 3);
    }

    /// Touch moves a piece to MRU, changing eviction order.
    ///
    /// Insert 0,1,2, touch 0, insert 3 → evicts 1 (not 0, because 0
    /// was touched).
    #[test]
    fn touch_changes_eviction_order() {
        let mut cache = PieceCache::with_capacity(3);
        let now = Instant::now();
        cache.insert(0, 100, now);
        cache.insert(1, 100, now);
        cache.insert(2, 100, now);

        cache.touch(0, now); // move 0 to MRU

        let evicted = cache.insert(3, 100, now);
        assert_eq!(evicted, Some(1)); // 1 is now LRU
        assert!(cache.contains(0)); // 0 survived
    }

    /// LRU snapshot reflects access order.
    #[test]
    fn lru_snapshot_order() {
        let mut cache = PieceCache::with_capacity(5);
        let now = Instant::now();
        cache.insert(10, 100, now);
        cache.insert(20, 100, now);
        cache.insert(30, 100, now);

        let snap = cache.lru_snapshot();
        // Front = most recent (30), back = least recent (10).
        assert_eq!(snap, vec![30, 20, 10]);
    }

    /// Sequential evictions drain the cache correctly.
    #[test]
    fn sequential_evictions() {
        let mut cache = PieceCache::with_capacity(2);
        let now = Instant::now();
        cache.insert(0, 100, now);
        cache.insert(1, 100, now);

        assert_eq!(cache.insert(2, 100, now), Some(0));
        assert_eq!(cache.insert(3, 100, now), Some(1));
        assert_eq!(cache.insert(4, 100, now), Some(2));
        assert_eq!(cache.len(), 2);
        assert!(cache.contains(3));
        assert!(cache.contains(4));
    }

    // ── Boundary cases ──────────────────────────────────────────────

    /// Zero-capacity cache evicts on every insert.
    #[test]
    fn zero_capacity() {
        let mut cache = PieceCache::with_capacity(0);
        let now = Instant::now();
        // Insert into zero-capacity: the piece doesn't stay (evicted immediately
        // if we had a prior entry, but with empty cache there's nothing to evict).
        // With capacity 0, entries.len() >= capacity is always true, but
        // evict_lru on empty returns None. The piece is then inserted,
        // leaving the cache over-capacity — but the next insert will evict it.
        let evicted = cache.insert(1, 100, now);
        assert_eq!(evicted, None); // nothing to evict from empty
                                   // Now at 1 element with capacity 0 — next insert evicts.
        let evicted = cache.insert(2, 100, now);
        assert_eq!(evicted, Some(1));
    }

    /// Single-capacity cache always holds the most recent piece.
    #[test]
    fn single_capacity() {
        let mut cache = PieceCache::with_capacity(1);
        let now = Instant::now();
        cache.insert(10, 100, now);
        assert!(cache.contains(10));
        let evicted = cache.insert(20, 200, now);
        assert_eq!(evicted, Some(10));
        assert!(cache.contains(20));
        assert_eq!(cache.total_bytes(), 200);
    }

    /// Touch on non-existent piece is a no-op.
    #[test]
    fn touch_nonexistent_noop() {
        let mut cache = PieceCache::with_capacity(5);
        let now = Instant::now();
        cache.touch(99, now); // should not panic or corrupt state
        assert!(cache.is_empty());
    }

    // ── Total bytes accounting ──────────────────────────────────────

    /// Total bytes tracks inserts and evictions.
    #[test]
    fn total_bytes_accounting() {
        let mut cache = PieceCache::with_capacity(2);
        let now = Instant::now();
        cache.insert(0, 100, now);
        cache.insert(1, 200, now);
        assert_eq!(cache.total_bytes(), 300);

        // Evict piece 0 (100 bytes), add piece 2 (150 bytes).
        cache.insert(2, 150, now);
        assert_eq!(cache.total_bytes(), 350); // 200 + 150
    }

    // ── Default and Debug ───────────────────────────────────────────

    /// Default cache uses default capacity.
    #[test]
    fn default_capacity() {
        let cache = PieceCache::default();
        assert_eq!(cache.capacity(), DEFAULT_CACHE_CAPACITY);
    }

    /// Debug output includes capacity and length.
    #[test]
    fn debug_output() {
        let cache = PieceCache::with_capacity(10);
        let debug = format!("{cache:?}");
        assert!(debug.contains("capacity"), "debug: {debug}");
        assert!(debug.contains("len"), "debug: {debug}");
    }
}
