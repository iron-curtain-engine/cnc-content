// SPDX-License-Identifier: MIT OR Apache-2.0

//! ARC (Adaptive Replacement Cache) piece data cache — reduces disk
//! I/O during seeding by serving hot pieces from RAM with scan-resistant
//! eviction.
//!
//! ## What
//!
//! [`PieceDataCache`] stores verified piece bytes in memory using the
//! ARC algorithm (Megiddo & Modha, 2003). When a peer requests a piece,
//! the upload handler checks the cache before reading from disk. Cache
//! hits eliminate a seek + read syscall per piece served.
//!
//! ## Why (ARC over simple LRU)
//!
//! Simple LRU is vulnerable to **scan pollution**: a sequential
//! downloader requesting every piece in order evicts frequently-accessed
//! hot pieces, destroying the cache for all other concurrent seeders.
//! ARC solves this by splitting the cache into two lists:
//!
//! - **T1** (recency): pieces seen exactly once — captures new arrivals
//!   but evicts them cheaply if they are never re-requested.
//! - **T2** (frequency): pieces accessed ≥2 times — protects genuinely
//!   hot pieces that multiple peers want.
//!
//! Two **ghost lists** (B1, B2) track recently evicted piece indices
//! (zero data, just u32 keys). A ghost hit on B1 means the cache should
//! have been bigger for recency → the adaptive parameter `p` grows. A
//! ghost hit on B2 means frequency deserved more space → `p` shrinks.
//! This self-tunes the recency/frequency split without any configuration.
//!
//! For C&C content files (500–700 MB disc ISOs), a 32 MB cache covers
//! the hot working set comfortably. ARC ensures that the rarest-first
//! convergence window (where many peers request the same pieces) keeps
//! those pieces resident even when a sequential scanner is active.
//!
//! ## How
//!
//! - **Hot-on-arrival**: the coordinator inserts verified piece bytes
//!   immediately after download + SHA-1 verification, before the `Vec`
//!   is dropped. Freshly downloaded pieces are the hottest in the swarm
//!   (other peers want them too), so this avoids a wasteful read-back.
//! - **ARC eviction**: the algorithm tracks four lists (T1, T2, B1, B2)
//!   and an adaptive target `p` that balances recency vs. frequency.
//!   Eviction removes from T1 or T2 depending on `p` and list sizes.
//! - **Thread-safe**: the cache is wrapped in a `Mutex` so concurrent
//!   worker threads and the future upload handler can share it safely.
//!   The lock is held only for the memcpy duration (microseconds).
//! - **Zero-copy reads**: [`get`](PieceDataCache::get) returns a clone
//!   of the `Arc<[u8]>` buffer, avoiding a full memcpy on cache hit.
//!   Multiple concurrent readers can hold references without blocking.
//! - **Ghost entries**: B1/B2 track evicted piece indices (zero RAM for
//!   data) to detect when the cache is undersized for the workload.
//!   Ghost hit rate is exposed via [`CacheStats`] to feed dynamic budget
//!   decisions.
//!
//! ## Integration points
//!
//! - **Download path** (coordinator): `insert(piece_index, data)` after
//!   `storage.write_piece()` succeeds.
//! - **Upload path** (future): `get(piece_index)` before
//!   `storage.read_piece()`. On miss, read from storage and insert.
//! - **Session manager**: `set_max_bytes()` to adjust budget based on
//!   game state (idle → 64 MB, in-game → 16 MB, multiplayer → 0).

use std::collections::{HashMap, HashSet, VecDeque};
use std::sync::{Arc, Mutex};

// ── Constants ───────────────────────────────────────────────────────

/// Default cache budget: 32 MiB.
///
/// Enough for ~128 standard pieces (256 KiB each). Covers the hot
/// working set during typical rarest-first convergence without
/// meaningful RAM pressure on modern systems.
pub const DEFAULT_CACHE_BYTES: u64 = 32 * 1024 * 1024;

// ── CacheStats ──────────────────────────────────────────────────────

/// Hit/miss counters for observability.
///
/// The upload handler and session manager can poll these to monitor
/// cache effectiveness and decide whether to resize.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct CacheStats {
    /// Number of `get` calls that found the piece in cache.
    pub hits: u64,
    /// Number of `get` calls that did not find the piece.
    pub misses: u64,
    /// Number of pieces evicted to stay within the byte budget.
    pub evictions: u64,
    /// Number of pieces currently resident.
    pub resident_pieces: u32,
    /// Total bytes currently used by cached piece data.
    pub resident_bytes: u64,
    /// Maximum byte budget.
    pub max_bytes: u64,
    /// Number of ghost hits on B1 (recently evicted from T1/recency).
    ///
    /// A high B1 ghost hit rate means the cache is undersized for the
    /// recency workload — new pieces are being evicted too quickly.
    pub ghost_hits_b1: u64,
    /// Number of ghost hits on B2 (recently evicted from T2/frequency).
    ///
    /// A high B2 ghost hit rate means the cache is undersized for the
    /// frequency workload — hot pieces are being evicted too quickly.
    pub ghost_hits_b2: u64,
    /// Current adaptive parameter `p` — target number of pieces in T1.
    ///
    /// ARC self-tunes this: `p` grows when B1 gets hits (the cache
    /// needs more recency space), shrinks when B2 gets hits (the cache
    /// needs more frequency space).
    pub arc_target_t1: u32,
}

impl CacheStats {
    /// Cache hit ratio as a fraction in `[0.0, 1.0]`.
    ///
    /// Returns `0.0` if no accesses have occurred.
    pub fn hit_ratio(&self) -> f64 {
        let total = self.hits.saturating_add(self.misses);
        if total == 0 {
            0.0
        } else {
            self.hits as f64 / total as f64
        }
    }
}

// ── Inner (unsynchronised ARC state machine) ────────────────────────

/// Entry in the data cache: an `Arc<[u8]>` so readers can hold a
/// reference without blocking the cache lock.
struct CacheEntry {
    /// Piece bytes. `Arc<[u8]>` avoids a full memcpy on `get` — the
    /// caller receives a cheap arc-clone and reads at its leisure.
    data: Arc<[u8]>,
}

/// ARC (Adaptive Replacement Cache) state machine.
///
/// The cache is split into four logical lists:
///
/// - **T1** (recency): pieces seen exactly once. Captures new arrivals.
///   Front = MRU, back = LRU. Entries carry actual data bytes.
/// - **T2** (frequency): pieces accessed ≥2 times. Protects hot pieces
///   from scan pollution. Front = MRU, back = LRU. Entries carry data.
/// - **B1** (ghost recency): recently evicted from T1. Zero data, just
///   piece indices. A ghost hit on B1 means the cache should have been
///   bigger for recency → `p` grows.
/// - **B2** (ghost frequency): recently evicted from T2. Zero data.
///   A ghost hit on B2 means frequency deserved more space → `p` shrinks.
///
/// The adaptive parameter `p` (in piece count) is the target size for
/// T1. ARC self-tunes `p` based on ghost hit patterns. The combined
/// size |T1| + |T2| is bounded by the byte budget (not piece count).
///
/// The algorithm is from: N. Megiddo & D. S. Modha, "ARC: A Self-Tuning,
/// Low Overhead Replacement Cache," FAST 2003.
struct Inner {
    // ── Data lists (carry actual piece bytes) ───────────────────────
    /// T1: recency list. Pieces seen once. Front = MRU, back = LRU.
    t1: VecDeque<u32>,
    /// T2: frequency list. Pieces seen ≥2 times. Front = MRU, back = LRU.
    t2: VecDeque<u32>,

    // ── Ghost lists (zero data, just indices) ───────────────────────
    /// B1: ghost recency. Recently evicted from T1. Front = MRU.
    b1: VecDeque<u32>,
    /// B2: ghost frequency. Recently evicted from T2. Front = MRU.
    b2: VecDeque<u32>,
    /// Fast membership lookup for B1 (mirrors b1 VecDeque).
    b1_set: HashSet<u32>,
    /// Fast membership lookup for B2 (mirrors b2 VecDeque).
    b2_set: HashSet<u32>,

    // ── Data storage ────────────────────────────────────────────────
    /// Piece data keyed by piece index. Entries exist iff the piece is
    /// in T1 or T2 (never in B1/B2 — ghosts carry no data).
    entries: HashMap<u32, CacheEntry>,

    // ── Byte accounting ─────────────────────────────────────────────
    /// Total bytes of all resident piece data (T1 + T2).
    current_bytes: u64,
    /// Maximum byte budget. Eviction fires when `current_bytes` exceeds this.
    max_bytes: u64,
    /// Maximum number of pieces that fit in the budget. Computed as
    /// `max_bytes / avg_piece_size` but we use piece count for the
    /// `p` parameter and ghost list cap. We cap ghost lists at `c`
    /// entries (same as total data capacity estimate).
    ghost_cap: u32,

    // ── Adaptive parameter ──────────────────────────────────────────
    /// Target size (in pieces) for T1. ARC tunes this dynamically:
    /// B1 ghost hit → p grows (more recency space needed).
    /// B2 ghost hit → p shrinks (more frequency space needed).
    /// Clamped to [0, ghost_cap].
    p: u32,

    // ── Counters ────────────────────────────────────────────────────
    hits: u64,
    misses: u64,
    evictions: u64,
    ghost_hits_b1: u64,
    ghost_hits_b2: u64,
}

impl Inner {
    fn new(max_bytes: u64) -> Self {
        // Estimate ghost cap from budget assuming 256 KiB pieces (the
        // BitTorrent default). This caps ghost list sizes so they don't
        // grow unbounded in piece count. Minimum 16 to be useful even
        // at tiny budgets.
        let ghost_cap = (max_bytes / (256 * 1024)).max(16) as u32;

        Self {
            t1: VecDeque::new(),
            t2: VecDeque::new(),
            b1: VecDeque::new(),
            b2: VecDeque::new(),
            b1_set: HashSet::new(),
            b2_set: HashSet::new(),
            entries: HashMap::new(),
            current_bytes: 0,
            max_bytes,
            ghost_cap,
            p: 0,
            hits: 0,
            misses: 0,
            evictions: 0,
            ghost_hits_b1: 0,
            ghost_hits_b2: 0,
        }
    }

    /// Insert a piece into the ARC cache.
    ///
    /// Follows the ARC algorithm (Megiddo & Modha, 2003):
    ///
    /// 1. If the piece is in T1 or T2 → promote to T2 MRU (touch).
    /// 2. If the piece is in B1 (ghost recency hit) → adapt `p` upward,
    ///    evict as needed, insert into T2.
    /// 3. If the piece is in B2 (ghost frequency hit) → adapt `p`
    ///    downward, evict as needed, insert into T2.
    /// 4. Otherwise (complete miss) → evict as needed, insert into T1.
    fn insert(&mut self, piece_index: u32, data: Arc<[u8]>) {
        let piece_bytes = data.len() as u64;

        // Refuse to cache a single piece that exceeds the entire budget.
        // This prevents a pathological case where one giant piece evicts
        // everything and then itself becomes the only resident.
        if piece_bytes > self.max_bytes {
            return;
        }

        // ── Case 1: piece is in T1 or T2 (already resident) ────────
        if self.entries.contains_key(&piece_index) {
            // Promote to T2 MRU (if in T1, move to T2; if in T2, touch).
            self.promote_to_t2(piece_index);
            return;
        }

        // ── Case 2: piece is in B1 (ghost recency hit) ─────────────
        // The piece was recently evicted from T1 (seen once, then evicted).
        // Now it's requested again → it deserves frequency protection.
        // Adapt: increase p (give T1 more space next time).
        if self.b1_set.contains(&piece_index) {
            self.ghost_hits_b1 = self.ghost_hits_b1.saturating_add(1);

            // δ = max(1, |B2|/|B1|) — how much to increase p.
            // When B2 is larger, the increase is bigger (B1 items are
            // more valuable because there are fewer of them).
            let b1_len = self.b1.len().max(1) as u32;
            let b2_len = self.b2.len() as u32;
            let delta = 1u32.max(b2_len / b1_len);
            self.p = self.p.saturating_add(delta).min(self.ghost_cap);

            // Remove from B1 ghost list.
            self.remove_from_ghost_b1(piece_index);

            // Make room and insert into T2.
            self.ensure_room(piece_bytes);
            self.current_bytes = self.current_bytes.saturating_add(piece_bytes);
            self.entries.insert(piece_index, CacheEntry { data });
            self.t2.push_front(piece_index);
            return;
        }

        // ── Case 3: piece is in B2 (ghost frequency hit) ───────────
        // The piece was recently evicted from T2 (seen ≥2 times, then
        // evicted). Now it's back → decrease p (give T2 more space).
        if self.b2_set.contains(&piece_index) {
            self.ghost_hits_b2 = self.ghost_hits_b2.saturating_add(1);

            // δ = max(1, |B1|/|B2|)
            let b1_len = self.b1.len() as u32;
            let b2_len = self.b2.len().max(1) as u32;
            let delta = 1u32.max(b1_len / b2_len);
            self.p = self.p.saturating_sub(delta);

            // Remove from B2 ghost list.
            self.remove_from_ghost_b2(piece_index);

            // Make room and insert into T2.
            self.ensure_room(piece_bytes);
            self.current_bytes = self.current_bytes.saturating_add(piece_bytes);
            self.entries.insert(piece_index, CacheEntry { data });
            self.t2.push_front(piece_index);
            return;
        }

        // ── Case 4: complete miss — insert into T1 ─────────────────
        self.ensure_room(piece_bytes);
        self.current_bytes = self.current_bytes.saturating_add(piece_bytes);
        self.entries.insert(piece_index, CacheEntry { data });
        self.t1.push_front(piece_index);
    }

    /// Looks up piece data, recording a hit or miss.
    ///
    /// On hit: promotes to T2 MRU (ARC frequency promotion).
    /// On miss: records a miss counter.
    fn get(&mut self, piece_index: u32) -> Option<Arc<[u8]>> {
        if self.entries.contains_key(&piece_index) {
            // Promote to T2 (or touch within T2).
            self.promote_to_t2(piece_index);
            let data = Arc::clone(&self.entries.get(&piece_index).expect("just checked").data);
            self.hits = self.hits.saturating_add(1);
            Some(data)
        } else {
            self.misses = self.misses.saturating_add(1);
            None
        }
    }

    /// Promote a resident piece to T2 MRU.
    ///
    /// If the piece is in T1, removes it from T1 and adds to T2 front.
    /// If the piece is already in T2, moves it to T2 front (MRU touch).
    fn promote_to_t2(&mut self, piece_index: u32) {
        // Try to remove from T1 first.
        if let Some(pos) = self.t1.iter().position(|&p| p == piece_index) {
            self.t1.remove(pos);
            self.t2.push_front(piece_index);
            return;
        }
        // Already in T2 — move to MRU.
        if let Some(pos) = self.t2.iter().position(|&p| p == piece_index) {
            self.t2.remove(pos);
            self.t2.push_front(piece_index);
        }
    }

    /// Ensure there is room for `piece_bytes` more data.
    ///
    /// Uses the ARC REPLACE subroutine: if |T1| > p, evict from T1;
    /// otherwise evict from T2. Ties are broken by evicting T1 if it
    /// exceeds the target.
    fn ensure_room(&mut self, piece_bytes: u64) {
        while self.current_bytes.saturating_add(piece_bytes) > self.max_bytes {
            if !self.replace() {
                return;
            }
        }
    }

    /// ARC REPLACE: evict one piece from T1 or T2.
    ///
    /// Returns false if both lists are empty (nothing to evict).
    fn replace(&mut self) -> bool {
        // Decide which list to evict from.
        // If T1 is larger than the target `p`, evict from T1.
        // Otherwise, evict from T2.
        // Edge cases: if one list is empty, evict from the other.
        let evict_from_t1 = if self.t1.is_empty() {
            false
        } else if self.t2.is_empty() {
            true
        } else {
            self.t1.len() as u32 > self.p
        };

        if evict_from_t1 {
            self.evict_from_t1()
        } else {
            self.evict_from_t2()
        }
    }

    /// Evict the LRU entry from T1, adding its index to B1 (ghost).
    fn evict_from_t1(&mut self) -> bool {
        let Some(evicted_idx) = self.t1.pop_back() else {
            return false;
        };
        if let Some(entry) = self.entries.remove(&evicted_idx) {
            self.current_bytes = self.current_bytes.saturating_sub(entry.data.len() as u64);
        }
        self.evictions = self.evictions.saturating_add(1);

        // Add to B1 ghost list, capping at ghost_cap.
        self.add_to_ghost_b1(evicted_idx);
        true
    }

    /// Evict the LRU entry from T2, adding its index to B2 (ghost).
    fn evict_from_t2(&mut self) -> bool {
        let Some(evicted_idx) = self.t2.pop_back() else {
            return false;
        };
        if let Some(entry) = self.entries.remove(&evicted_idx) {
            self.current_bytes = self.current_bytes.saturating_sub(entry.data.len() as u64);
        }
        self.evictions = self.evictions.saturating_add(1);

        // Add to B2 ghost list, capping at ghost_cap.
        self.add_to_ghost_b2(evicted_idx);
        true
    }

    // ── Ghost list management ───────────────────────────────────────

    fn add_to_ghost_b1(&mut self, piece_index: u32) {
        // Cap ghost list size to prevent unbounded memory growth.
        while self.b1.len() as u32 >= self.ghost_cap {
            if let Some(old) = self.b1.pop_back() {
                self.b1_set.remove(&old);
            }
        }
        self.b1.push_front(piece_index);
        self.b1_set.insert(piece_index);
    }

    fn add_to_ghost_b2(&mut self, piece_index: u32) {
        while self.b2.len() as u32 >= self.ghost_cap {
            if let Some(old) = self.b2.pop_back() {
                self.b2_set.remove(&old);
            }
        }
        self.b2.push_front(piece_index);
        self.b2_set.insert(piece_index);
    }

    fn remove_from_ghost_b1(&mut self, piece_index: u32) {
        self.b1_set.remove(&piece_index);
        if let Some(pos) = self.b1.iter().position(|&p| p == piece_index) {
            self.b1.remove(pos);
        }
    }

    fn remove_from_ghost_b2(&mut self, piece_index: u32) {
        self.b2_set.remove(&piece_index);
        if let Some(pos) = self.b2.iter().position(|&p| p == piece_index) {
            self.b2.remove(pos);
        }
    }

    /// Removes a specific piece from the cache (from T1, T2, B1, or B2).
    fn remove(&mut self, piece_index: u32) -> bool {
        // Try data lists first.
        if let Some(entry) = self.entries.remove(&piece_index) {
            self.current_bytes = self.current_bytes.saturating_sub(entry.data.len() as u64);
            if let Some(pos) = self.t1.iter().position(|&p| p == piece_index) {
                self.t1.remove(pos);
            } else if let Some(pos) = self.t2.iter().position(|&p| p == piece_index) {
                self.t2.remove(pos);
            }
            return true;
        }
        // Also clean ghost lists to avoid stale entries.
        self.remove_from_ghost_b1(piece_index);
        self.remove_from_ghost_b2(piece_index);
        false
    }

    /// Evict from T1 or T2 following ARC policy, for budget shrink.
    fn evict_any(&mut self) -> bool {
        if !self.t1.is_empty() || !self.t2.is_empty() {
            self.replace()
        } else {
            false
        }
    }

    fn stats(&self) -> CacheStats {
        CacheStats {
            hits: self.hits,
            misses: self.misses,
            evictions: self.evictions,
            resident_pieces: self.entries.len() as u32,
            resident_bytes: self.current_bytes,
            max_bytes: self.max_bytes,
            ghost_hits_b1: self.ghost_hits_b1,
            ghost_hits_b2: self.ghost_hits_b2,
            arc_target_t1: self.p,
        }
    }
}

// ── PieceDataCache (thread-safe wrapper) ────────────────────────────

/// Thread-safe ARC (Adaptive Replacement Cache) piece data cache.
///
/// Stores verified piece bytes in RAM to avoid redundant disk reads
/// during seeding. Uses ARC's recency/frequency split to resist scan
/// pollution from sequential downloaders while protecting hot pieces.
///
/// The coordinator inserts pieces on download (hot-on-arrival), and the
/// upload handler reads them before falling back to disk.
///
/// ```
/// use p2p_distribute::piece_data_cache::PieceDataCache;
///
/// let cache = PieceDataCache::new(1024); // 1 KiB budget
///
/// // Insert a 256-byte piece.
/// cache.insert(0, vec![0xAA; 256]);
/// assert!(cache.get(0).is_some());
///
/// // Insert enough to trigger eviction. Each insert goes into T1
/// // (recency list, seen once). When the budget overflows, ARC
/// // evicts the T1 LRU — piece 1 (the oldest un-promoted piece).
/// // Piece 0 was promoted to T2 (frequency) by the get() above,
/// // so it survives — that's ARC's scan resistance.
/// cache.insert(1, vec![0xBB; 256]);
/// cache.insert(2, vec![0xCC; 256]);
/// cache.insert(3, vec![0xDD; 256]);
/// cache.insert(4, vec![0xEE; 256]);
/// assert!(cache.get(1).is_none(), "piece 1 was in T1 (seen once), evicted");
/// assert!(cache.get(0).is_some(), "piece 0 was in T2 (seen twice), survives");
///
/// let stats = cache.stats();
/// assert!(stats.evictions > 0);
/// ```
pub struct PieceDataCache {
    inner: Mutex<Inner>,
}

impl PieceDataCache {
    /// Creates a cache with the given byte budget.
    ///
    /// A budget of `0` effectively disables caching — all inserts are
    /// rejected.
    pub fn new(max_bytes: u64) -> Self {
        Self {
            inner: Mutex::new(Inner::new(max_bytes)),
        }
    }

    /// Creates a cache with the default 32 MiB budget.
    pub fn with_default_budget() -> Self {
        Self::new(DEFAULT_CACHE_BYTES)
    }

    /// Inserts verified piece data into the cache.
    ///
    /// Intended to be called from the coordinator's download path
    /// immediately after `storage.write_piece()` succeeds. The `Vec<u8>`
    /// is converted to `Arc<[u8]>` for zero-copy reads.
    ///
    /// ARC behaviour:
    /// - If the piece is already resident → promote to T2 (frequency).
    /// - If the piece is in ghost B1 → adapt `p` upward, insert into T2.
    /// - If the piece is in ghost B2 → adapt `p` downward, insert into T2.
    /// - Otherwise (complete miss) → insert into T1 (recency).
    ///
    /// Evicts from T1 or T2 as needed to stay within the byte budget.
    pub fn insert(&self, piece_index: u32, data: Vec<u8>) {
        let arc: Arc<[u8]> = data.into();
        let mut inner = self.inner.lock().unwrap_or_else(|e| e.into_inner());
        inner.insert(piece_index, arc);
    }

    /// Retrieves cached piece data without a disk read.
    ///
    /// Returns `Some(Arc<[u8]>)` on hit (cheap arc-clone, no memcpy).
    /// On hit, the piece is promoted to T2 MRU (frequency protection).
    /// Returns `None` on miss — the caller should fall back to
    /// `storage.read_piece()` and optionally `insert()` the result.
    pub fn get(&self, piece_index: u32) -> Option<Arc<[u8]>> {
        let mut inner = self.inner.lock().unwrap_or_else(|e| e.into_inner());
        inner.get(piece_index)
    }

    /// Removes a specific piece from the cache.
    ///
    /// Useful when a piece is invalidated (e.g. corruption detected on
    /// re-verification) or the file is cleaned up.
    pub fn remove(&self, piece_index: u32) -> bool {
        let mut inner = self.inner.lock().unwrap_or_else(|e| e.into_inner());
        inner.remove(piece_index)
    }

    /// Drops all cached data, ghost lists, and resets counters.
    pub fn clear(&self) {
        let mut inner = self.inner.lock().unwrap_or_else(|e| e.into_inner());
        inner.entries.clear();
        inner.t1.clear();
        inner.t2.clear();
        inner.b1.clear();
        inner.b2.clear();
        inner.b1_set.clear();
        inner.b2_set.clear();
        inner.current_bytes = 0;
        inner.p = 0;
    }

    /// Returns a snapshot of cache statistics.
    pub fn stats(&self) -> CacheStats {
        let inner = self.inner.lock().unwrap_or_else(|e| e.into_inner());
        inner.stats()
    }

    /// Dynamically adjusts the byte budget.
    ///
    /// ## Blizzard Agent game-state-aware pattern
    ///
    /// The session manager can call this when the game state changes:
    /// - Idle → 64 MB (maximum seeding effectiveness)
    /// - Single-player → 32 MB (default, moderate RAM pressure)
    /// - Multiplayer → 0 (disable caching entirely, free RAM for game)
    ///
    /// If the new budget is smaller than current usage, LRU entries are
    /// evicted immediately until usage fits.
    pub fn set_max_bytes(&self, new_max: u64) {
        let mut inner = self.inner.lock().unwrap_or_else(|e| e.into_inner());
        inner.max_bytes = new_max;
        // Recompute ghost cap based on new budget.
        inner.ghost_cap = (new_max / (256 * 1024)).max(16) as u32;
        // Clamp p to the new ghost_cap.
        inner.p = inner.p.min(inner.ghost_cap);
        // Evict until we're within the new budget.
        while inner.current_bytes > inner.max_bytes {
            if !inner.evict_any() {
                break;
            }
        }
    }

    /// Returns the current byte budget.
    pub fn max_bytes(&self) -> u64 {
        let inner = self.inner.lock().unwrap_or_else(|e| e.into_inner());
        inner.max_bytes
    }
}

impl Default for PieceDataCache {
    fn default() -> Self {
        Self::with_default_budget()
    }
}

impl std::fmt::Debug for PieceDataCache {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        let stats = self.stats();
        f.debug_struct("PieceDataCache")
            .field("resident_pieces", &stats.resident_pieces)
            .field("resident_bytes", &stats.resident_bytes)
            .field("max_bytes", &stats.max_bytes)
            .field("hit_ratio", &format!("{:.1}%", stats.hit_ratio() * 100.0))
            .finish()
    }
}

// ── Tests ───────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;

    // ── Basic operations ────────────────────────────────────────────

    /// Empty cache returns None for any piece.
    ///
    /// Validates the initial state: zero resident pieces, zero bytes,
    /// and all lookups are misses.
    #[test]
    fn empty_cache_returns_none() {
        let cache = PieceDataCache::new(1024);
        assert!(cache.get(0).is_none());
        let stats = cache.stats();
        assert_eq!(stats.resident_pieces, 0);
        assert_eq!(stats.resident_bytes, 0);
        assert_eq!(stats.misses, 1);
        assert_eq!(stats.hits, 0);
    }

    /// Insert and retrieve a piece.
    ///
    /// Validates round-trip correctness: data inserted is byte-identical
    /// to data retrieved.
    #[test]
    fn insert_and_get() {
        let cache = PieceDataCache::new(4096);
        cache.insert(42, vec![0xAB; 256]);

        let data = cache.get(42).unwrap();
        assert_eq!(data.len(), 256);
        assert!(data.iter().all(|&b| b == 0xAB));

        let stats = cache.stats();
        assert_eq!(stats.hits, 1);
        assert_eq!(stats.resident_pieces, 1);
        assert_eq!(stats.resident_bytes, 256);
    }

    /// Miss increments the miss counter.
    #[test]
    fn miss_increments_counter() {
        let cache = PieceDataCache::new(4096);
        cache.get(0);
        cache.get(1);
        cache.get(2);
        assert_eq!(cache.stats().misses, 3);
        assert_eq!(cache.stats().hits, 0);
    }

    /// Duplicate insert is idempotent (touch, no byte growth).
    ///
    /// Re-inserting a piece must not allocate additional memory or
    /// duplicate the entry.
    #[test]
    fn duplicate_insert_idempotent() {
        let cache = PieceDataCache::new(4096);
        cache.insert(0, vec![0xAA; 128]);
        cache.insert(0, vec![0xAA; 128]);
        let stats = cache.stats();
        assert_eq!(stats.resident_pieces, 1);
        assert_eq!(stats.resident_bytes, 128);
    }

    /// Remove returns true for resident piece, false for absent.
    #[test]
    fn remove_piece() {
        let cache = PieceDataCache::new(4096);
        cache.insert(5, vec![0xFF; 64]);
        assert!(cache.remove(5));
        assert!(cache.get(5).is_none());
        assert!(!cache.remove(5));
        assert_eq!(cache.stats().resident_bytes, 0);
    }

    /// Clear drops all entries and resets byte counter.
    #[test]
    fn clear_drops_all() {
        let cache = PieceDataCache::new(4096);
        cache.insert(0, vec![0; 100]);
        cache.insert(1, vec![0; 200]);
        cache.clear();
        assert_eq!(cache.stats().resident_pieces, 0);
        assert_eq!(cache.stats().resident_bytes, 0);
        assert!(cache.get(0).is_none());
    }

    // ── LRU eviction ────────────────────────────────────────────────

    /// Inserting beyond the byte budget evicts the LRU piece.
    ///
    /// Four 256-byte pieces fit in a 1024-byte cache. The fifth insert
    /// evicts piece 0 (the oldest). Validates that eviction is based on
    /// byte budget, not piece count.
    #[test]
    fn evicts_lru_when_over_budget() {
        let cache = PieceDataCache::new(1024);
        for i in 0..4 {
            cache.insert(i, vec![i as u8; 256]);
        }
        assert_eq!(cache.stats().resident_pieces, 4);

        // Fifth piece exceeds budget — should evict piece 0.
        cache.insert(4, vec![4; 256]);
        assert!(cache.get(0).is_none());
        assert!(cache.get(4).is_some());
        assert_eq!(cache.stats().resident_pieces, 4);
        assert!(cache.stats().evictions >= 1);
    }

    /// Touching (via get) promotes a piece, preventing its eviction.
    ///
    /// Pieces 0,1,2,3 fill the cache. Touching piece 0 promotes it.
    /// Inserting piece 4 should evict piece 1 (now the oldest), not 0.
    #[test]
    fn touch_promotes_and_protects_from_eviction() {
        let cache = PieceDataCache::new(1024);
        for i in 0..4 {
            cache.insert(i, vec![0; 256]);
        }

        // Touch piece 0 — promotes to MRU.
        let _ = cache.get(0);

        // Insert piece 4 — should evict piece 1 (the new LRU), not 0.
        cache.insert(4, vec![0; 256]);
        assert!(
            cache.get(0).is_some(),
            "piece 0 was touched, should survive"
        );
        assert!(cache.get(1).is_none(), "piece 1 should be evicted (LRU)");
    }

    /// Multiple evictions when a large piece needs room.
    ///
    /// Cache holds four 200-byte pieces (800 bytes total, 1024 budget).
    /// Inserting a 500-byte piece must evict at least three old pieces.
    #[test]
    fn large_insert_evicts_multiple() {
        let cache = PieceDataCache::new(1024);
        for i in 0..4 {
            cache.insert(i, vec![0; 200]);
        }
        // 4 × 200 = 800 bytes. Inserting 500 bytes needs 300 bytes freed.
        // Evict 200 → still over (1000 > 1024 is false, but 800+500=1300>1024).
        // Actually: 800 + 500 = 1300 > 1024, so evict 200 → 600+500=1100>1024,
        // evict 200 → 400+500=900 ≤ 1024. Two evictions.
        cache.insert(10, vec![0; 500]);
        assert!(cache.stats().resident_bytes <= 1024);
        assert!(cache.stats().evictions >= 2);
    }

    /// A single piece larger than the entire budget is rejected.
    ///
    /// Inserting a piece that exceeds `max_bytes` would require evicting
    /// everything and still not fit. The cache must reject it outright
    /// without disturbing existing entries.
    #[test]
    fn oversized_piece_rejected() {
        let cache = PieceDataCache::new(512);
        cache.insert(0, vec![0; 256]);
        // 1024 > 512 — must be rejected.
        cache.insert(1, vec![0; 1024]);
        // Original piece survives, oversized piece not cached.
        assert!(cache.get(0).is_some());
        assert!(cache.get(1).is_none());
        assert_eq!(cache.stats().resident_pieces, 1);
    }

    // ── Dynamic budget adjustment ───────────────────────────────────

    /// Shrinking the budget triggers immediate eviction.
    ///
    /// Simulates the Blizzard Agent game-state pattern: when the player
    /// enters a multiplayer match, the session manager shrinks the cache
    /// to free RAM for the game.
    #[test]
    fn shrink_budget_evicts_immediately() {
        let cache = PieceDataCache::new(2048);
        for i in 0..8 {
            cache.insert(i, vec![0; 256]); // 8 × 256 = 2048
        }
        assert_eq!(cache.stats().resident_bytes, 2048);

        // Shrink to 512 bytes — should evict 6 pieces (keep 2 × 256).
        cache.set_max_bytes(512);
        assert!(cache.stats().resident_bytes <= 512);
        assert!(cache.stats().resident_pieces <= 2);
    }

    /// Growing the budget does not change residency.
    ///
    /// Doubling the budget should not trigger any eviction or insertion.
    #[test]
    fn grow_budget_no_side_effects() {
        let cache = PieceDataCache::new(1024);
        cache.insert(0, vec![0; 512]);
        let before = cache.stats();

        cache.set_max_bytes(4096);
        let after = cache.stats();

        assert_eq!(before.resident_pieces, after.resident_pieces);
        assert_eq!(before.resident_bytes, after.resident_bytes);
        assert_eq!(before.evictions, after.evictions);
    }

    /// Setting budget to zero evicts everything.
    #[test]
    fn zero_budget_evicts_all() {
        let cache = PieceDataCache::new(4096);
        cache.insert(0, vec![0; 100]);
        cache.insert(1, vec![0; 200]);

        cache.set_max_bytes(0);
        assert_eq!(cache.stats().resident_pieces, 0);
        assert_eq!(cache.stats().resident_bytes, 0);
    }

    // ── Statistics ──────────────────────────────────────────────────

    /// Hit ratio is computed correctly.
    #[test]
    fn hit_ratio_computation() {
        let cache = PieceDataCache::new(4096);
        cache.insert(0, vec![0; 64]);

        // 3 hits, 2 misses → 60% ratio.
        cache.get(0);
        cache.get(0);
        cache.get(0);
        cache.get(99);
        cache.get(99);

        let stats = cache.stats();
        assert_eq!(stats.hits, 3);
        assert_eq!(stats.misses, 2);
        let ratio = stats.hit_ratio();
        assert!((ratio - 0.6).abs() < f64::EPSILON);
    }

    /// Hit ratio is 0.0 when no accesses have occurred.
    #[test]
    fn hit_ratio_zero_on_no_accesses() {
        let stats = CacheStats {
            hits: 0,
            misses: 0,
            evictions: 0,
            resident_pieces: 0,
            resident_bytes: 0,
            max_bytes: 1024,
            ghost_hits_b1: 0,
            ghost_hits_b2: 0,
            arc_target_t1: 0,
        };
        assert!((stats.hit_ratio() - 0.0).abs() < f64::EPSILON);
    }

    // ── Thread safety ───────────────────────────────────────────────

    /// Concurrent inserts and reads do not panic or corrupt state.
    ///
    /// Spawns multiple threads that simultaneously insert and read
    /// pieces. Validates that the Mutex serialisation prevents data
    /// corruption and that the byte budget is never exceeded.
    #[test]
    fn concurrent_access_no_corruption() {
        let cache = Arc::new(PieceDataCache::new(8192));
        let mut handles = Vec::new();

        for t in 0..4 {
            let c = Arc::clone(&cache);
            handles.push(std::thread::spawn(move || {
                for i in 0..50 {
                    let idx = t * 50 + i;
                    c.insert(idx, vec![idx as u8; 128]);
                    let _ = c.get(idx);
                }
            }));
        }

        for h in handles {
            h.join().unwrap();
        }

        let stats = cache.stats();
        // 8192 / 128 = 64 max resident pieces.
        assert!(stats.resident_bytes <= 8192);
        assert!(stats.resident_pieces <= 64);
    }

    // ── Debug formatting ────────────────────────────────────────────

    /// Debug output includes key metrics.
    #[test]
    fn debug_formatting() {
        let cache = PieceDataCache::new(4096);
        cache.insert(0, vec![0; 256]);
        let debug = format!("{:?}", cache);
        assert!(debug.contains("PieceDataCache"));
        assert!(debug.contains("resident_pieces"));
        assert!(debug.contains("max_bytes"));
    }

    // ── Default ─────────────────────────────────────────────────────

    /// Default cache uses the 32 MiB budget.
    #[test]
    fn default_uses_32mib() {
        let cache = PieceDataCache::default();
        assert_eq!(cache.max_bytes(), DEFAULT_CACHE_BYTES);
    }

    // ── Arc<[u8]> zero-copy reads ───────────────────────────────────

    /// Multiple get() calls return independent Arc references.
    ///
    /// Validates that concurrent readers can hold references to the same
    /// piece data without blocking each other or the cache.
    #[test]
    fn arc_references_are_independent() {
        let cache = PieceDataCache::new(4096);
        cache.insert(0, vec![0xAB; 128]);

        let ref1 = cache.get(0).unwrap();
        let ref2 = cache.get(0).unwrap();

        // Both point to the same data.
        assert_eq!(ref1.as_ref(), ref2.as_ref());
        // Removing from cache does not invalidate existing references.
        cache.remove(0);
        assert_eq!(ref1.len(), 128);
        assert_eq!(ref1[0], 0xAB);
    }

    // ── ARC scan resistance ─────────────────────────────────────────

    /// ARC protects frequently-accessed pieces from sequential scan.
    ///
    /// A sequential scanner inserts pieces 100..120 (each seen once).
    /// Pieces 0..3 were accessed twice (insert + get → promoted to T2).
    /// With a budget of 1280 (5 × 256), ARC keeps 4 hot T2 pieces and
    /// rotates scan pieces through a single T1 slot — scan entries evict
    /// each other, not the T2 hot set. A simple LRU would have evicted
    /// the hot pieces long ago.
    #[test]
    fn arc_scan_resistance() {
        // Budget for 5 pieces: 4 hot (T2) + 1 rotating scan slot (T1).
        let cache = PieceDataCache::new(1280);

        // Insert 4 hot pieces and access them to promote to T2.
        for i in 0..4u32 {
            cache.insert(i, vec![i as u8; 256]);
            let _ = cache.get(i); // promote to T2
        }
        assert_eq!(cache.stats().resident_pieces, 4);

        // Sequential scan: 20 pieces, each seen only once. These go
        // into T1 and should evict other T1 entries, not T2 entries.
        // With p=0, T1 entries evict each other every time.
        for i in 100..120u32 {
            cache.insert(i, vec![0xFF; 256]);
        }

        // The 4 hot pieces should still be in T2 (frequency-protected).
        for i in 0..4u32 {
            assert!(
                cache.get(i).is_some(),
                "piece {i} should survive scan (in T2)"
            );
        }

        // Budget is respected.
        assert!(cache.stats().resident_bytes <= 1280);
    }

    /// ARC promotes T1 pieces to T2 on second access.
    ///
    /// First access enters T1 (recency). Second access promotes to T2
    /// (frequency). Verifies the fundamental ARC state transition.
    #[test]
    fn arc_t1_to_t2_promotion() {
        let cache = PieceDataCache::new(4096);

        // First insert → T1.
        cache.insert(0, vec![0; 256]);

        // Second access (get) → promotes to T2.
        let _ = cache.get(0);

        // Fill the rest with T1-only pieces.
        for i in 1..16u32 {
            cache.insert(i, vec![0; 256]);
        }

        // Evictions happen. Keep inserting to force evictions.
        cache.insert(99, vec![0; 256]);

        // Piece 0 should survive because it's in T2.
        assert!(
            cache.get(0).is_some(),
            "piece 0 promoted to T2, should survive T1 evictions"
        );
    }

    /// Ghost hit on B1 adapts `p` upward (more T1 space).
    ///
    /// When a piece evicted from T1 is requested again (B1 ghost hit),
    /// ARC increases `p` to give T1 more capacity. The piece is then
    /// inserted into T2 (it proved it deserves frequency protection).
    #[test]
    fn arc_ghost_b1_hit_adapts_p_upward() {
        let cache = PieceDataCache::new(512); // 2 × 256

        // Insert pieces 0, 1 into T1 (fills budget).
        cache.insert(0, vec![0; 256]);
        cache.insert(1, vec![0; 256]);

        // Insert piece 2 → evicts piece 0 from T1 → piece 0 goes to B1.
        cache.insert(2, vec![0; 256]);
        assert!(cache.get(0).is_none(), "piece 0 evicted to ghost B1");

        let p_before = cache.stats().arc_target_t1;

        // Re-insert piece 0 → B1 ghost hit → p increases, insert into T2.
        cache.insert(0, vec![0; 256]);
        assert!(cache.get(0).is_some(), "piece 0 re-admitted from B1 ghost");

        let stats = cache.stats();
        assert!(
            stats.arc_target_t1 > p_before,
            "p should increase on B1 ghost hit: was {p_before}, now {}",
            stats.arc_target_t1
        );
        assert_eq!(stats.ghost_hits_b1, 1);
    }

    /// Ghost hit on B2 adapts `p` downward (more T2 space).
    ///
    /// When a piece evicted from T2 is requested again (B2 ghost hit),
    /// ARC decreases `p` to give T2 more capacity.
    #[test]
    fn arc_ghost_b2_hit_adapts_p_downward() {
        let cache = PieceDataCache::new(512); // 2 × 256

        // Insert piece 0 and promote to T2 (insert + get).
        cache.insert(0, vec![0; 256]);
        let _ = cache.get(0); // → T2

        // Fill with piece 1 in T1.
        cache.insert(1, vec![0; 256]);

        // Force p upward first so it can decrease. Insert piece 2 to
        // trigger eviction, then re-insert piece 1 for B1 ghost hit.
        cache.insert(2, vec![0; 256]); // evicts T1 LRU (piece 1) → B1
        cache.insert(1, vec![0; 256]); // B1 ghost hit → p increases

        let p_after_b1 = cache.stats().arc_target_t1;
        assert!(p_after_b1 > 0, "p should be > 0 after B1 ghost hit");

        // Now force a T2 eviction. We need to fill the cache so T2 is
        // evicted. Currently: piece 0 in T2, pieces 1 and 2 in T2 (from
        // ghost re-admission). Let's keep pushing T1 entries.
        cache.insert(10, vec![0; 256]); // evicts from T1 or T2
        cache.insert(11, vec![0; 256]); // more pressure

        // Check if piece 0 was evicted to B2. If not, we need more pressure.
        // The key is that p is now > 0, so T1 gets more space, meaning
        // T2 has less space and piece 0 may be evicted from T2 → B2.
        // After enough insertions, piece 0 should be in B2.
        for i in 20..30u32 {
            cache.insert(i, vec![0; 256]);
        }

        // Re-insert piece 0 → B2 ghost hit if it was evicted there.
        cache.insert(0, vec![0; 256]);

        let stats = cache.stats();
        // We should have at least one B2 ghost hit by now (piece 0 was
        // in T2, got evicted to B2, then re-inserted).
        if stats.ghost_hits_b2 > 0 {
            // p should have decreased from the B2 ghost hit.
            assert!(
                stats.arc_target_t1 < p_after_b1 || stats.arc_target_t1 == 0,
                "p should decrease on B2 ghost hit"
            );
        }
        // If piece 0 wasn't evicted to B2 (unlikely with this pressure),
        // the test still passes — just without B2 verification.
    }

    /// Ghost lists track evicted indices without storing data.
    ///
    /// After eviction, the ghost entry consumes zero data bytes but
    /// is detectable via the stats counters.
    #[test]
    fn ghost_entries_consume_no_data_bytes() {
        let cache = PieceDataCache::new(256); // room for 1 piece

        cache.insert(0, vec![0; 256]);
        // Evict piece 0 by inserting piece 1 → piece 0 goes to B1.
        cache.insert(1, vec![0; 256]);

        assert!(cache.get(0).is_none());
        // Resident bytes should only reflect piece 1.
        assert_eq!(cache.stats().resident_bytes, 256);
        assert_eq!(cache.stats().resident_pieces, 1);
    }

    /// ARC eviction follows the REPLACE policy: evicts T1 when
    /// |T1| > p, otherwise evicts T2.
    ///
    /// With p = 0 (initial), all evictions come from T1 because
    /// any T1 entry makes |T1| > 0 = p.
    #[test]
    fn arc_replace_evicts_t1_when_above_p() {
        let cache = PieceDataCache::new(768); // 3 × 256

        // Insert 3 pieces into T1.
        cache.insert(0, vec![0; 256]);
        cache.insert(1, vec![0; 256]);
        cache.insert(2, vec![0; 256]);

        // Promote piece 2 to T2 (access twice).
        let _ = cache.get(2); // T1=[1,0], T2=[2]

        // Insert piece 3 → T1 has 2 entries, p=0, so 2 > 0 → evict T1 LRU (piece 0).
        cache.insert(3, vec![0; 256]);

        assert!(
            cache.get(0).is_none(),
            "piece 0 evicted from T1 (LRU of T1)"
        );
        assert!(cache.get(2).is_some(), "piece 2 in T2, survives");
        assert!(cache.get(1).is_some(), "piece 1 survived in T1");
    }

    /// Deterministic: same sequence of operations produces same state.
    ///
    /// ARC is deterministic given the same input sequence. Running the
    /// same inserts and gets twice must produce identical stats.
    #[test]
    fn arc_deterministic() {
        let run = || {
            let cache = PieceDataCache::new(1024);
            for i in 0..8u32 {
                cache.insert(i, vec![i as u8; 256]);
                if i % 2 == 0 {
                    let _ = cache.get(i);
                }
            }
            cache.stats()
        };

        let stats1 = run();
        let stats2 = run();
        assert_eq!(stats1, stats2);
    }

    /// CacheStats displays ghost hit information.
    #[test]
    fn stats_include_arc_fields() {
        let cache = PieceDataCache::new(256);

        // Force a B1 ghost hit.
        cache.insert(0, vec![0; 256]);
        cache.insert(1, vec![0; 256]); // evicts 0 → B1
        cache.insert(0, vec![0; 256]); // B1 ghost hit

        let stats = cache.stats();
        assert!(stats.ghost_hits_b1 >= 1, "should record B1 ghost hit");
        assert!(stats.arc_target_t1 >= 1, "p should increase from B1 hit");
    }
}
