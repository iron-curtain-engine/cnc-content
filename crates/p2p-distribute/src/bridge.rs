//! P2P-to-HTTP bridge node — mirrors swarm content to HTTP clients.
//!
//! A bridge node participates in a BitTorrent-like swarm as a peer but also
//! serves content to plain HTTP clients that cannot speak the P2P protocol.
//! This module provides the orchestration types that connect the demand
//! tracker (`demand.rs`), piece cache (`cache.rs`), gateway HTTP handling
//! (`gateway.rs`), and mirror health monitoring into a single coherent
//! bridge abstraction.
//!
//! # Architecture
//!
//! ```text
//!   HTTP client ──► BridgeNode ──► PieceCache (LRU, bounded)
//!                       │               ▲
//!                       │               │
//!                       ▼               │
//!                  DemandTracker ──► prefetch from swarm / mirrors
//!                                       │
//!                                  MirrorPool (φ-detector health)
//! ```
//!
//! The bridge tracks request heat via `DemandTracker`, caches served pieces
//! in `PieceCache`, and pre-fetches hot pieces from the healthiest HTTP
//! mirrors tracked by `MirrorPool`.  Mirror health is monitored using the
//! phi accrual failure detector — the same mechanism used for peer liveness.

use std::time::Instant;

use crate::cache::PieceCache;
use crate::demand::DemandTracker;
use crate::phi_detector::PhiDetector;

// ── Constants ────────────────────────────────────────────────────────

/// Default maximum number of HTTP mirrors tracked by a bridge node.
pub const DEFAULT_MAX_MIRRORS: usize = 32;

/// Default phi threshold above which a mirror is considered suspect.
///
/// Uses the same value as `phi_detector::SUSPECT_PHI_THRESHOLD` (3.0)
/// because mirror health monitoring has the same semantics as peer
/// liveness detection — a phi of 3.0 means ~5% chance the mirror is
/// actually healthy.
pub const MIRROR_SUSPECT_PHI: f64 = 3.0;

/// Default phi threshold above which a mirror is considered dead.
///
/// Mirrors above this threshold are skipped during selection.
pub const MIRROR_DEAD_PHI: f64 = 8.0;

/// Default number of hot pieces to prefetch ahead of demand.
pub const DEFAULT_PREFETCH_COUNT: usize = 8;

// ── Error ────────────────────────────────────────────────────────────

/// Errors that can occur during bridge operations.
#[derive(Debug, thiserror::Error)]
pub enum BridgeError {
    /// Mirror pool is full — cannot add more mirrors.
    #[error("mirror pool full: {max} mirrors already tracked")]
    MirrorPoolFull {
        /// Maximum number of mirrors the pool can hold.
        max: usize,
    },

    /// Mirror URL already exists in the pool.
    #[error("duplicate mirror: {url}")]
    DuplicateMirror {
        /// The URL that was already present.
        url: String,
    },

    /// Mirror not found for the given URL.
    #[error("mirror not found: {url}")]
    MirrorNotFound {
        /// The URL that was looked up.
        url: String,
    },

    /// Requested piece is not in the cache.
    #[error("piece {piece_index} not cached")]
    PieceNotCached {
        /// The piece index that was requested.
        piece_index: u32,
    },
}

// ── MirrorHealth ─────────────────────────────────────────────────────

/// Health and performance state of a single HTTP mirror.
///
/// Each mirror is monitored by a `PhiDetector` that tracks response
/// inter-arrival times. A mirror's phi score rises when responses slow
/// down or stop, and falls when the mirror responds consistently.
#[derive(Debug, Clone)]
pub struct MirrorHealth {
    /// The base URL of the mirror (e.g. `https://mirror.example.com/content/`).
    url: String,

    /// Phi accrual failure detector tracking response liveness.
    detector: PhiDetector,

    /// Cumulative bytes successfully received from this mirror.
    bytes_served: u64,

    /// Number of successful responses from this mirror.
    success_count: u64,

    /// Number of failed responses (HTTP errors, timeouts) from this mirror.
    failure_count: u64,
}

impl MirrorHealth {
    /// Creates a new mirror health tracker.
    fn new(url: String, now: Instant) -> Self {
        Self {
            url,
            detector: PhiDetector::new(now),
            bytes_served: 0,
            success_count: 0,
            failure_count: 0,
        }
    }

    /// Returns the mirror's base URL.
    pub fn url(&self) -> &str {
        &self.url
    }

    /// Records a successful response from this mirror.
    ///
    /// Updates the phi detector's heartbeat window and accumulates
    /// throughput statistics.
    pub fn record_success(&mut self, bytes: u64, now: Instant) {
        self.detector.record_heartbeat(now);
        self.bytes_served = self.bytes_served.saturating_add(bytes);
        self.success_count = self.success_count.saturating_add(1);
    }

    /// Records a failed response (timeout, HTTP error, connection refused).
    pub fn record_failure(&mut self) {
        self.failure_count = self.failure_count.saturating_add(1);
    }

    /// Returns the current phi score for this mirror.
    ///
    /// Higher phi means the mirror is less likely to be alive. Returns
    /// 0.0 if insufficient samples have been collected.
    pub fn phi(&self, now: Instant) -> f64 {
        self.detector.phi(now)
    }

    /// Returns `true` if the mirror's phi is below the given threshold.
    pub fn is_available(&self, now: Instant, threshold: f64) -> bool {
        self.detector.is_available(now, threshold)
    }

    /// Total bytes successfully served by this mirror.
    pub fn bytes_served(&self) -> u64 {
        self.bytes_served
    }

    /// Number of successful responses.
    pub fn success_count(&self) -> u64 {
        self.success_count
    }

    /// Number of failed responses.
    pub fn failure_count(&self) -> u64 {
        self.failure_count
    }
}

// ── MirrorPool ───────────────────────────────────────────────────────

/// Pool of HTTP mirrors with phi-detector-based health monitoring.
///
/// The pool tracks multiple mirrors and provides selection based on
/// availability (phi score below threshold). Mirrors that exceed the
/// dead threshold are excluded from selection but remain tracked so
/// they can recover if they start responding again.
#[derive(Debug)]
pub struct MirrorPool {
    /// Tracked mirrors, ordered by insertion.
    mirrors: Vec<MirrorHealth>,

    /// Maximum number of mirrors this pool can hold.
    max_mirrors: usize,

    /// Phi score above which a mirror is considered dead for selection.
    dead_threshold: f64,
}

impl MirrorPool {
    /// Creates a new empty mirror pool with the default capacity.
    pub fn new() -> Self {
        Self {
            mirrors: Vec::new(),
            max_mirrors: DEFAULT_MAX_MIRRORS,
            dead_threshold: MIRROR_DEAD_PHI,
        }
    }

    /// Creates a mirror pool with the given capacity limit.
    pub fn with_capacity(max_mirrors: usize) -> Self {
        Self {
            mirrors: Vec::with_capacity(max_mirrors),
            max_mirrors,
            dead_threshold: MIRROR_DEAD_PHI,
        }
    }

    /// Sets the phi threshold above which mirrors are considered dead.
    pub fn set_dead_threshold(&mut self, threshold: f64) {
        self.dead_threshold = threshold;
    }

    /// Adds a mirror to the pool.
    ///
    /// Returns an error if the pool is full or the URL is already tracked.
    pub fn add_mirror(&mut self, url: String, now: Instant) -> Result<(), BridgeError> {
        if self.mirrors.len() >= self.max_mirrors {
            return Err(BridgeError::MirrorPoolFull {
                max: self.max_mirrors,
            });
        }
        // Check for duplicate URL.
        if self.mirrors.iter().any(|m| m.url == url) {
            return Err(BridgeError::DuplicateMirror { url });
        }
        self.mirrors.push(MirrorHealth::new(url, now));
        Ok(())
    }

    /// Removes a mirror by URL. Returns the removed health state.
    pub fn remove_mirror(&mut self, url: &str) -> Result<MirrorHealth, BridgeError> {
        let pos = self
            .mirrors
            .iter()
            .position(|m| m.url == url)
            .ok_or_else(|| BridgeError::MirrorNotFound {
                url: url.to_owned(),
            })?;
        Ok(self.mirrors.swap_remove(pos))
    }

    /// Returns a reference to the health state for the given mirror URL.
    pub fn get(&self, url: &str) -> Option<&MirrorHealth> {
        self.mirrors.iter().find(|m| m.url == url)
    }

    /// Returns a mutable reference to the health state for the given mirror URL.
    pub fn get_mut(&mut self, url: &str) -> Option<&mut MirrorHealth> {
        self.mirrors.iter_mut().find(|m| m.url == url)
    }

    /// Returns all mirrors whose phi score is below the dead threshold,
    /// sorted by phi ascending (healthiest first).
    pub fn available_mirrors(&self, now: Instant) -> Vec<&MirrorHealth> {
        let mut available: Vec<&MirrorHealth> = self
            .mirrors
            .iter()
            .filter(|m| m.is_available(now, self.dead_threshold))
            .collect();
        // Sort by phi ascending — healthiest mirror first.
        available.sort_by(|a, b| {
            a.phi(now)
                .partial_cmp(&b.phi(now))
                .unwrap_or(std::cmp::Ordering::Equal)
        });
        available
    }

    /// Returns the number of tracked mirrors.
    pub fn mirror_count(&self) -> usize {
        self.mirrors.len()
    }

    /// Returns the maximum capacity of this pool.
    pub fn max_mirrors(&self) -> usize {
        self.max_mirrors
    }

    /// Returns all tracked mirrors, regardless of health.
    pub fn all_mirrors(&self) -> &[MirrorHealth] {
        &self.mirrors
    }
}

impl Default for MirrorPool {
    fn default() -> Self {
        Self::new()
    }
}

// ── BridgeConfig ─────────────────────────────────────────────────────

/// Configuration for a bridge node.
///
/// Controls cache capacity, prefetch aggressiveness, and mirror health
/// thresholds. All fields have sensible defaults.
#[derive(Debug, Clone)]
pub struct BridgeConfig {
    /// Maximum number of pieces to hold in the LRU cache.
    pub cache_capacity: u32,

    /// Number of hot pieces to prefetch from mirrors ahead of demand.
    pub prefetch_count: usize,

    /// Phi score above which a mirror is considered suspect (logged).
    pub suspect_phi: f64,

    /// Phi score above which a mirror is excluded from selection.
    pub dead_phi: f64,

    /// Half-life in seconds for the demand decay function.
    pub demand_half_life_secs: f64,

    /// Maximum mirrors to track.
    pub max_mirrors: usize,
}

impl Default for BridgeConfig {
    fn default() -> Self {
        Self {
            cache_capacity: crate::cache::DEFAULT_CACHE_CAPACITY,
            prefetch_count: DEFAULT_PREFETCH_COUNT,
            suspect_phi: MIRROR_SUSPECT_PHI,
            dead_phi: MIRROR_DEAD_PHI,
            demand_half_life_secs: crate::demand::DEFAULT_DECAY_HALF_LIFE_SECS,
            max_mirrors: DEFAULT_MAX_MIRRORS,
        }
    }
}

// ── PrefetchPlan ─────────────────────────────────────────────────────

/// A prefetch recommendation produced by `BridgeNode::plan_prefetch`.
///
/// Lists piece indices the bridge should fetch from the swarm or mirrors
/// in order to satisfy anticipated demand, sorted by descending heat.
#[derive(Debug, Clone, PartialEq)]
pub struct PrefetchPlan {
    /// Pieces to fetch, ordered by descending heat (hottest first).
    pub pieces: Vec<u32>,
}

// ── BridgeNode ───────────────────────────────────────────────────────

/// Orchestrates a P2P-to-HTTP bridge node.
///
/// `BridgeNode` composes `DemandTracker`, `PieceCache`, and `MirrorPool`
/// to provide a unified interface for bridge operations:
///
/// - Recording HTTP client requests (updates demand + touches cache).
/// - Planning prefetch work (identifies hot uncached pieces).
/// - Managing mirror health (add, remove, record outcomes).
/// - Querying cache residency.
///
/// The actual I/O (HTTP serving, swarm protocol, mirror fetching) is
/// handled by the caller. `BridgeNode` is a pure-logic orchestrator.
#[derive(Debug)]
pub struct BridgeNode {
    /// Tracks request heat for demand-driven prefetch.
    demand: DemandTracker,

    /// Bounded LRU piece cache.
    cache: PieceCache,

    /// HTTP mirror health pool.
    mirrors: MirrorPool,

    /// Number of pieces to prefetch ahead of demand.
    prefetch_count: usize,

    /// Phi threshold for suspect logging.
    suspect_phi: f64,
}

impl BridgeNode {
    /// Creates a new bridge node with default configuration.
    pub fn new() -> Self {
        Self::with_config(BridgeConfig::default())
    }

    /// Creates a bridge node with the given configuration.
    pub fn with_config(config: BridgeConfig) -> Self {
        let mut mirrors = MirrorPool::with_capacity(config.max_mirrors);
        mirrors.set_dead_threshold(config.dead_phi);
        Self {
            demand: DemandTracker::with_half_life(config.demand_half_life_secs),
            cache: PieceCache::with_capacity(config.cache_capacity),
            mirrors,
            prefetch_count: config.prefetch_count,
            suspect_phi: config.suspect_phi,
        }
    }

    /// Records that an HTTP client requested a piece.
    ///
    /// Updates the demand tracker's heat score and touches the piece in
    /// the cache (if present) to move it to MRU position.
    pub fn record_request(&mut self, piece_index: u32, now: Instant) {
        self.demand.record_request(piece_index, now);
        self.cache.touch(piece_index, now);
    }

    /// Returns `true` if the piece is present in the cache.
    pub fn has_piece(&self, piece_index: u32) -> bool {
        self.cache.contains(piece_index)
    }

    /// Inserts a fetched piece into the cache.
    ///
    /// Returns the evicted piece index if the cache was full. The caller
    /// is responsible for cleaning up evicted piece data from storage.
    pub fn cache_piece(&mut self, piece_index: u32, size_bytes: u32, now: Instant) -> Option<u32> {
        self.cache.insert(piece_index, size_bytes, now)
    }

    /// Plans which pieces to prefetch based on current demand.
    ///
    /// Returns up to `prefetch_count` piece indices that are hot (above
    /// the cold threshold) but not yet in the cache, sorted by
    /// descending heat.
    pub fn plan_prefetch(&self, now: Instant) -> PrefetchPlan {
        // Fetch more candidates than needed in case many are already cached.
        let candidates = self
            .demand
            .hottest_pieces(self.prefetch_count.saturating_mul(2), now);
        let pieces: Vec<u32> = candidates
            .into_iter()
            .filter(|(idx, _heat)| !self.cache.contains(*idx))
            .take(self.prefetch_count)
            .map(|(idx, _heat)| idx)
            .collect();
        PrefetchPlan { pieces }
    }

    /// Returns the best available mirror URL for fetching a piece.
    ///
    /// Selects the mirror with the lowest phi score that is below the
    /// dead threshold. Returns `None` if no mirrors are available.
    pub fn best_mirror(&self, now: Instant) -> Option<&str> {
        self.mirrors.available_mirrors(now).first().map(|m| m.url())
    }

    /// Returns all mirrors currently in the suspect range.
    ///
    /// A mirror is suspect if its phi is above `suspect_phi` but below
    /// the dead threshold. These are degraded but not yet excluded.
    pub fn suspect_mirrors(&self, now: Instant) -> Vec<&str> {
        self.mirrors
            .all_mirrors()
            .iter()
            .filter(|m| {
                let phi = m.phi(now);
                phi >= self.suspect_phi && m.is_available(now, self.mirrors.dead_threshold)
            })
            .map(|m| m.url())
            .collect()
    }

    /// Returns a reference to the mirror pool.
    pub fn mirrors(&self) -> &MirrorPool {
        &self.mirrors
    }

    /// Returns a mutable reference to the mirror pool.
    pub fn mirrors_mut(&mut self) -> &mut MirrorPool {
        &mut self.mirrors
    }

    /// Returns a reference to the demand tracker.
    pub fn demand(&self) -> &DemandTracker {
        &self.demand
    }

    /// Returns a reference to the piece cache.
    pub fn cache(&self) -> &PieceCache {
        &self.cache
    }

    /// Returns the total bytes currently held in the cache.
    pub fn cached_bytes(&self) -> u64 {
        self.cache.total_bytes()
    }

    /// Returns the number of pieces currently in the cache.
    pub fn cached_pieces(&self) -> u32 {
        self.cache.len()
    }
}

impl Default for BridgeNode {
    fn default() -> Self {
        Self::new()
    }
}

// ── Tests ────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;
    use std::time::{Duration, Instant};

    // ── MirrorHealth tests ───────────────────────────────────────────

    /// A new mirror starts with zero counters and low phi.
    #[test]
    fn mirror_health_initial_state() {
        let now = Instant::now();
        let mirror = MirrorHealth::new("https://m1.example.com/".into(), now);
        assert_eq!(mirror.url(), "https://m1.example.com/");
        assert_eq!(mirror.bytes_served(), 0);
        assert_eq!(mirror.success_count(), 0);
        assert_eq!(mirror.failure_count(), 0);
        // Phi is 0.0 with insufficient samples.
        assert!((mirror.phi(now) - 0.0).abs() < f64::EPSILON);
    }

    /// Recording successes updates counters and feeds the phi detector.
    #[test]
    fn mirror_health_tracks_successes() {
        let now = Instant::now();
        let mut mirror = MirrorHealth::new("https://m1.example.com/".into(), now);
        mirror.record_success(1024, now + Duration::from_millis(100));
        mirror.record_success(2048, now + Duration::from_millis(200));
        assert_eq!(mirror.bytes_served(), 3072);
        assert_eq!(mirror.success_count(), 2);
        assert_eq!(mirror.failure_count(), 0);
    }

    /// Recording failures increments only the failure counter.
    #[test]
    fn mirror_health_tracks_failures() {
        let now = Instant::now();
        let mut mirror = MirrorHealth::new("https://m1.example.com/".into(), now);
        mirror.record_failure();
        mirror.record_failure();
        assert_eq!(mirror.failure_count(), 2);
        assert_eq!(mirror.success_count(), 0);
    }

    /// Counters saturate rather than overflow.
    #[test]
    fn mirror_health_counters_saturate() {
        let now = Instant::now();
        let mut mirror = MirrorHealth::new("https://m.example.com/".into(), now);
        mirror.bytes_served = u64::MAX;
        mirror.record_success(100, now + Duration::from_millis(10));
        assert_eq!(mirror.bytes_served(), u64::MAX);
    }

    // ── MirrorPool tests ─────────────────────────────────────────────

    /// A new pool starts empty.
    #[test]
    fn mirror_pool_starts_empty() {
        let pool = MirrorPool::new();
        assert_eq!(pool.mirror_count(), 0);
        assert_eq!(pool.max_mirrors(), DEFAULT_MAX_MIRRORS);
    }

    /// Adding mirrors succeeds up to the capacity limit.
    #[test]
    fn mirror_pool_add_and_query() {
        let now = Instant::now();
        let mut pool = MirrorPool::with_capacity(2);
        pool.add_mirror("https://a.example.com/".into(), now)
            .unwrap();
        pool.add_mirror("https://b.example.com/".into(), now)
            .unwrap();
        assert_eq!(pool.mirror_count(), 2);
        assert!(pool.get("https://a.example.com/").is_some());
        assert!(pool.get("https://c.example.com/").is_none());
    }

    /// Adding beyond capacity returns `MirrorPoolFull`.
    #[test]
    fn mirror_pool_rejects_when_full() {
        let now = Instant::now();
        let mut pool = MirrorPool::with_capacity(1);
        pool.add_mirror("https://a.example.com/".into(), now)
            .unwrap();
        let err = pool
            .add_mirror("https://b.example.com/".into(), now)
            .unwrap_err();
        assert!(matches!(err, BridgeError::MirrorPoolFull { max: 1 }));
    }

    /// Duplicate URLs are rejected.
    #[test]
    fn mirror_pool_rejects_duplicate() {
        let now = Instant::now();
        let mut pool = MirrorPool::new();
        pool.add_mirror("https://a.example.com/".into(), now)
            .unwrap();
        let err = pool
            .add_mirror("https://a.example.com/".into(), now)
            .unwrap_err();
        assert!(matches!(err, BridgeError::DuplicateMirror { .. }));
    }

    /// Removing a mirror returns its health state.
    #[test]
    fn mirror_pool_remove() {
        let now = Instant::now();
        let mut pool = MirrorPool::new();
        pool.add_mirror("https://a.example.com/".into(), now)
            .unwrap();
        let removed = pool.remove_mirror("https://a.example.com/").unwrap();
        assert_eq!(removed.url(), "https://a.example.com/");
        assert_eq!(pool.mirror_count(), 0);
    }

    /// Removing a nonexistent mirror returns `MirrorNotFound`.
    #[test]
    fn mirror_pool_remove_not_found() {
        let mut pool = MirrorPool::new();
        let err = pool.remove_mirror("https://nope.example.com/").unwrap_err();
        assert!(matches!(err, BridgeError::MirrorNotFound { .. }));
    }

    /// Available mirrors are sorted by phi ascending (healthiest first).
    ///
    /// Both mirrors receive heartbeats so they have enough samples for
    /// meaningful phi values. The healthy mirror keeps responding; the
    /// slow mirror stops, causing its phi to rise.
    #[test]
    fn mirror_pool_available_sorted_by_phi() {
        let now = Instant::now();
        let mut pool = MirrorPool::new();
        pool.add_mirror("https://healthy.example.com/".into(), now)
            .unwrap();
        pool.add_mirror("https://slow.example.com/".into(), now)
            .unwrap();

        // Both mirrors respond for the first 5 heartbeats (100ms apart).
        for i in 1..=5 {
            let t = now + Duration::from_millis(100 * i);
            pool.get_mut("https://healthy.example.com/")
                .unwrap()
                .record_success(100, t);
            pool.get_mut("https://slow.example.com/")
                .unwrap()
                .record_success(100, t);
        }

        // Healthy mirror keeps responding at the same cadence through
        // the evaluation time. Slow mirror goes silent after 500ms.
        for i in 6..=20 {
            let t = now + Duration::from_millis(100 * i);
            pool.get_mut("https://healthy.example.com/")
                .unwrap()
                .record_success(100, t);
        }

        // Evaluate shortly after the last healthy heartbeat. The slow
        // mirror has been silent for 1500ms (15× its mean interval),
        // driving its phi well above the healthy mirror's.
        let t = now + Duration::from_millis(2100);
        let available = pool.available_mirrors(t);
        assert!(!available.is_empty());
        // The healthy mirror (low phi) should sort before the slow one.
        assert_eq!(available[0].url(), "https://healthy.example.com/");
    }

    /// Default mirrors pool implements Default.
    #[test]
    fn mirror_pool_default() {
        let pool = MirrorPool::default();
        assert_eq!(pool.mirror_count(), 0);
        assert_eq!(pool.max_mirrors(), DEFAULT_MAX_MIRRORS);
    }

    // ── BridgeConfig tests ───────────────────────────────────────────

    /// Default config has sensible values.
    #[test]
    fn bridge_config_defaults() {
        let config = BridgeConfig::default();
        assert_eq!(config.cache_capacity, crate::cache::DEFAULT_CACHE_CAPACITY);
        assert_eq!(config.prefetch_count, DEFAULT_PREFETCH_COUNT);
        assert!((config.suspect_phi - MIRROR_SUSPECT_PHI).abs() < f64::EPSILON);
        assert!((config.dead_phi - MIRROR_DEAD_PHI).abs() < f64::EPSILON);
        assert_eq!(config.max_mirrors, DEFAULT_MAX_MIRRORS);
    }

    // ── BridgeNode tests ─────────────────────────────────────────────

    /// A new bridge node starts with empty cache and no mirrors.
    #[test]
    fn bridge_node_starts_empty() {
        let node = BridgeNode::new();
        assert_eq!(node.cached_pieces(), 0);
        assert_eq!(node.cached_bytes(), 0);
        assert_eq!(node.mirrors().mirror_count(), 0);
    }

    /// Recording a request updates demand and touches cache if present.
    #[test]
    fn bridge_node_record_request_updates_demand() {
        let now = Instant::now();
        let mut node = BridgeNode::new();
        node.record_request(42, now);
        assert!(node.demand().is_hot(42, now));
    }

    /// Caching a piece makes it available via `has_piece`.
    #[test]
    fn bridge_node_cache_piece() {
        let now = Instant::now();
        let mut node = BridgeNode::new();
        assert!(!node.has_piece(10));
        let evicted = node.cache_piece(10, 256, now);
        assert!(evicted.is_none());
        assert!(node.has_piece(10));
        assert_eq!(node.cached_pieces(), 1);
        assert_eq!(node.cached_bytes(), 256);
    }

    /// Cache eviction returns the evicted piece index.
    #[test]
    fn bridge_node_cache_eviction() {
        let now = Instant::now();
        let config = BridgeConfig {
            cache_capacity: 2,
            ..BridgeConfig::default()
        };
        let mut node = BridgeNode::with_config(config);
        node.cache_piece(1, 100, now);
        node.cache_piece(2, 200, now + Duration::from_millis(1));
        // Cache is full. Inserting piece 3 evicts the LRU piece (1).
        let evicted = node.cache_piece(3, 300, now + Duration::from_millis(2));
        assert_eq!(evicted, Some(1));
        assert!(!node.has_piece(1));
        assert!(node.has_piece(2));
        assert!(node.has_piece(3));
    }

    /// Prefetch plan skips pieces already in the cache.
    #[test]
    fn bridge_node_prefetch_skips_cached() {
        let now = Instant::now();
        let config = BridgeConfig {
            prefetch_count: 3,
            ..BridgeConfig::default()
        };
        let mut node = BridgeNode::with_config(config);

        // Create demand for pieces 1, 2, 3.
        node.record_request(1, now);
        node.record_request(2, now);
        node.record_request(3, now);

        // Cache piece 2.
        node.cache_piece(2, 100, now);

        let plan = node.plan_prefetch(now);
        // Piece 2 should be excluded from the prefetch plan.
        assert!(!plan.pieces.contains(&2));
        // Pieces 1 and 3 should be in the plan.
        assert!(plan.pieces.contains(&1));
        assert!(plan.pieces.contains(&3));
    }

    /// Prefetch plan is limited to `prefetch_count` entries.
    #[test]
    fn bridge_node_prefetch_limited() {
        let now = Instant::now();
        let config = BridgeConfig {
            prefetch_count: 2,
            ..BridgeConfig::default()
        };
        let mut node = BridgeNode::with_config(config);

        for i in 0..10 {
            node.record_request(i, now);
        }
        let plan = node.plan_prefetch(now);
        assert!(plan.pieces.len() <= 2);
    }

    /// Best mirror returns the healthiest mirror URL.
    #[test]
    fn bridge_node_best_mirror() {
        let now = Instant::now();
        let mut node = BridgeNode::new();
        node.mirrors_mut()
            .add_mirror("https://m1.example.com/".into(), now)
            .unwrap();
        // With only one mirror and low phi, it should be available.
        assert_eq!(node.best_mirror(now), Some("https://m1.example.com/"));
    }

    /// Best mirror returns None when no mirrors are tracked.
    #[test]
    fn bridge_node_best_mirror_none() {
        let now = Instant::now();
        let node = BridgeNode::new();
        assert_eq!(node.best_mirror(now), None);
    }

    /// Suspect mirrors returns mirrors in the suspect phi range.
    ///
    /// With fresh mirrors that have no heartbeat history, phi stays at
    /// 0.0 (insufficient samples), so none should be suspect.
    #[test]
    fn bridge_node_no_suspects_initially() {
        let now = Instant::now();
        let mut node = BridgeNode::new();
        node.mirrors_mut()
            .add_mirror("https://m1.example.com/".into(), now)
            .unwrap();
        assert!(node.suspect_mirrors(now).is_empty());
    }

    /// Default BridgeNode implements Default trait.
    #[test]
    fn bridge_node_default() {
        let node = BridgeNode::default();
        assert_eq!(node.cached_pieces(), 0);
    }

    // ── Error display tests ──────────────────────────────────────────

    /// Error display messages include relevant context.
    #[test]
    fn error_display_includes_context() {
        let err = BridgeError::MirrorPoolFull { max: 32 };
        let msg = err.to_string();
        assert!(msg.contains("32"));

        let err = BridgeError::DuplicateMirror {
            url: "https://x.example.com/".into(),
        };
        assert!(err.to_string().contains("https://x.example.com/"));

        let err = BridgeError::MirrorNotFound {
            url: "https://y.example.com/".into(),
        };
        assert!(err.to_string().contains("https://y.example.com/"));

        let err = BridgeError::PieceNotCached { piece_index: 42 };
        assert!(err.to_string().contains("42"));
    }

    // ── PrefetchPlan tests ───────────────────────────────────────────

    /// Empty prefetch plan when there is no demand.
    #[test]
    fn prefetch_plan_empty_without_demand() {
        let now = Instant::now();
        let node = BridgeNode::new();
        let plan = node.plan_prefetch(now);
        assert!(plan.pieces.is_empty());
    }

    /// PrefetchPlan supports equality comparison.
    #[test]
    fn prefetch_plan_eq() {
        let a = PrefetchPlan {
            pieces: vec![1, 2, 3],
        };
        let b = PrefetchPlan {
            pieces: vec![1, 2, 3],
        };
        assert_eq!(a, b);
    }
}
