// SPDX-License-Identifier: MIT OR Apache-2.0

//! Demand-driven piece fetching — tracks swarm request patterns for
//! proactive prefetching.
//!
//! ## What
//!
//! `DemandTracker` monitors which pieces the swarm is requesting and
//! computes a priority score for each piece. A bridge node uses this
//! to proactively fetch hot pieces from HTTP mirrors *before* peers
//! ask for them, reducing latency.
//!
//! ## Why (P2P-to-HTTP bridge design)
//!
//! A bridge node participates in the P2P swarm but sources data from
//! HTTP mirrors. Without demand tracking, the bridge must wait for
//! each piece request before fetching from HTTP — adding a full
//! round-trip of latency. By monitoring request frequency, the bridge
//! can prefetch pieces that are trending in the swarm.
//!
//! ## How
//!
//! Each piece has a `DemandEntry` that tracks request count and
//! timestamps. The tracker computes a **heat score** using exponential
//! decay: recent requests contribute more than old ones. Pieces are
//! ranked by heat score for prefetch scheduling.
//!
//! The decay constant ensures that a piece that was hot 10 minutes ago
//! but is no longer requested will fade, freeing prefetch bandwidth
//! for currently popular pieces.

use std::collections::HashMap;
use std::time::Instant;

// ── Constants ───────────────────────────────────────────────────────

/// Default decay half-life in seconds.
///
/// After this many seconds, a request's contribution to the heat score
/// is halved. 60 seconds matches the typical piece exchange interval.
pub const DEFAULT_DECAY_HALF_LIFE_SECS: f64 = 60.0;

/// Minimum heat score below which a piece is considered cold.
///
/// Cold pieces are not worth prefetching. The threshold is set just
/// above zero to avoid floating-point noise.
pub const COLD_THRESHOLD: f64 = 0.01;

// ── DemandEntry ─────────────────────────────────────────────────────

/// Per-piece demand tracking state.
#[derive(Debug, Clone)]
struct DemandEntry {
    /// Cumulative heat score (decayed over time).
    heat: f64,
    /// Timestamp of the last request (for decay calculation).
    last_request: Instant,
    /// Total request count (for statistics, not decay).
    total_requests: u64,
}

// ── DemandTracker ───────────────────────────────────────────────────

/// Tracks piece demand across the swarm for prefetch scheduling.
///
/// ## Usage
///
/// 1. Call [`record_request`] when a swarm peer requests a piece.
/// 2. Call [`hottest_pieces`] to get the top-N pieces ranked by heat.
/// 3. Feed the result into the bridge's prefetch queue.
///
/// ```
/// use p2p_distribute::demand::DemandTracker;
/// use std::time::Instant;
///
/// let mut tracker = DemandTracker::new();
/// let now = Instant::now();
///
/// // Swarm peers request piece 42 frequently.
/// tracker.record_request(42, now);
/// tracker.record_request(42, now);
/// tracker.record_request(7, now);
///
/// let hot = tracker.hottest_pieces(2, now);
/// assert_eq!(hot[0].0, 42); // piece 42 is hottest
/// ```
pub struct DemandTracker {
    /// Per-piece demand state.
    entries: HashMap<u32, DemandEntry>,
    /// Decay half-life in seconds. Controls how fast old requests fade.
    decay_half_life_secs: f64,
    /// Pre-computed decay constant: ln(2) / half_life.
    decay_lambda: f64,
}

impl DemandTracker {
    /// Creates a new tracker with the default decay half-life.
    pub fn new() -> Self {
        Self::with_half_life(DEFAULT_DECAY_HALF_LIFE_SECS)
    }

    /// Creates a tracker with a custom decay half-life (in seconds).
    ///
    /// Shorter half-lives make the tracker more responsive to recent
    /// requests but forget history faster. Longer half-lives smooth
    /// out transient spikes.
    pub fn with_half_life(half_life_secs: f64) -> Self {
        // Clamp to a sane minimum to avoid division by zero or negative.
        let clamped = half_life_secs.max(0.1);
        Self {
            entries: HashMap::new(),
            decay_half_life_secs: clamped,
            decay_lambda: std::f64::consts::LN_2 / clamped,
        }
    }

    /// Records a piece request from a swarm peer.
    ///
    /// Each request adds 1.0 to the piece's heat score (after decaying
    /// the existing score to the current time).
    pub fn record_request(&mut self, piece_index: u32, now: Instant) {
        let lambda = self.decay_lambda;
        let entry = self.entries.entry(piece_index).or_insert(DemandEntry {
            heat: 0.0,
            last_request: now,
            total_requests: 0,
        });

        // Decay existing heat to current time before adding new request.
        let elapsed = now.duration_since(entry.last_request).as_secs_f64();
        entry.heat *= (-lambda * elapsed).exp();
        entry.heat += 1.0;
        entry.last_request = now;
        entry.total_requests = entry.total_requests.saturating_add(1);
    }

    /// Returns the current heat score for a piece (decayed to `now`).
    ///
    /// Returns 0.0 for untracked pieces.
    pub fn heat(&self, piece_index: u32, now: Instant) -> f64 {
        let Some(entry) = self.entries.get(&piece_index) else {
            return 0.0;
        };
        let elapsed = now.duration_since(entry.last_request).as_secs_f64();
        entry.heat * (-self.decay_lambda * elapsed).exp()
    }

    /// Returns whether a piece is considered "hot" (above cold threshold).
    pub fn is_hot(&self, piece_index: u32, now: Instant) -> bool {
        self.heat(piece_index, now) > COLD_THRESHOLD
    }

    /// Returns the top N hottest pieces, sorted by descending heat.
    ///
    /// Pieces below [`COLD_THRESHOLD`] are excluded. Returns fewer than
    /// N if not enough hot pieces exist.
    pub fn hottest_pieces(&self, n: usize, now: Instant) -> Vec<(u32, f64)> {
        let mut scored: Vec<(u32, f64)> = self
            .entries
            .iter()
            .map(|(&idx, entry)| {
                let elapsed = now.duration_since(entry.last_request).as_secs_f64();
                let score = entry.heat * (-self.decay_lambda * elapsed).exp();
                (idx, score)
            })
            .filter(|(_, score)| *score > COLD_THRESHOLD)
            .collect();

        // Sort descending by heat score. Use total_bits for deterministic
        // ordering of equal scores.
        scored.sort_by(|a, b| b.1.total_cmp(&a.1));
        scored.truncate(n);
        scored
    }

    /// Returns the total number of tracked pieces (including cold ones).
    pub fn tracked_count(&self) -> usize {
        self.entries.len()
    }

    /// Returns the total request count for a piece (not decayed, cumulative).
    pub fn total_requests(&self, piece_index: u32) -> u64 {
        self.entries
            .get(&piece_index)
            .map(|e| e.total_requests)
            .unwrap_or(0)
    }

    /// Removes cold entries to free memory.
    ///
    /// Pieces with heat below [`COLD_THRESHOLD`] are evicted. Call
    /// periodically (e.g. every minute) to prevent unbounded growth.
    pub fn evict_cold(&mut self, now: Instant) {
        let lambda = self.decay_lambda;
        self.entries.retain(|_, entry| {
            let elapsed = now.duration_since(entry.last_request).as_secs_f64();
            let score = entry.heat * (-lambda * elapsed).exp();
            score > COLD_THRESHOLD
        });
    }

    /// Returns the configured decay half-life in seconds.
    pub fn decay_half_life_secs(&self) -> f64 {
        self.decay_half_life_secs
    }
}

impl Default for DemandTracker {
    fn default() -> Self {
        Self::new()
    }
}

impl std::fmt::Debug for DemandTracker {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("DemandTracker")
            .field("tracked_count", &self.entries.len())
            .field("decay_half_life_secs", &self.decay_half_life_secs)
            .finish()
    }
}

// ── Tests ───────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;
    use std::time::Duration;

    // ── Basic tracking ──────────────────────────────────────────────

    /// New tracker starts empty.
    #[test]
    fn tracker_starts_empty() {
        let tracker = DemandTracker::new();
        assert_eq!(tracker.tracked_count(), 0);
    }

    /// Recording a request makes the piece hot.
    #[test]
    fn record_request_makes_hot() {
        let mut tracker = DemandTracker::new();
        let now = Instant::now();
        tracker.record_request(5, now);
        assert!(tracker.is_hot(5, now));
        assert_eq!(tracker.total_requests(5), 1);
    }

    /// Multiple requests increase heat.
    ///
    /// Each request adds 1.0 to the heat score (before decay), so
    /// 3 requests at the same instant should produce heat ≈ 3.0.
    #[test]
    fn multiple_requests_increase_heat() {
        let mut tracker = DemandTracker::new();
        let now = Instant::now();
        tracker.record_request(10, now);
        tracker.record_request(10, now);
        tracker.record_request(10, now);
        let heat = tracker.heat(10, now);
        assert!((heat - 3.0).abs() < 0.01, "heat should be ~3.0: {heat}");
        assert_eq!(tracker.total_requests(10), 3);
    }

    /// Untracked pieces have zero heat.
    #[test]
    fn untracked_piece_zero_heat() {
        let tracker = DemandTracker::new();
        assert_eq!(tracker.heat(99, Instant::now()), 0.0);
        assert!(!tracker.is_hot(99, Instant::now()));
        assert_eq!(tracker.total_requests(99), 0);
    }

    // ── Decay ───────────────────────────────────────────────────────

    /// Heat decays over time (exponential decay).
    ///
    /// After one half-life, the heat should be approximately halved.
    #[test]
    fn heat_decays_over_time() {
        let half_life = 10.0; // 10 seconds for testability
        let mut tracker = DemandTracker::with_half_life(half_life);
        let t0 = Instant::now();
        tracker.record_request(1, t0);

        let t1 = t0 + Duration::from_secs(10);
        let heat = tracker.heat(1, t1);
        // After one half-life: heat ≈ 0.5.
        assert!(
            (heat - 0.5).abs() < 0.05,
            "after one half-life heat should be ~0.5: {heat}"
        );
    }

    /// Heat approaches zero after many half-lives.
    #[test]
    fn heat_approaches_zero() {
        let mut tracker = DemandTracker::with_half_life(1.0);
        let t0 = Instant::now();
        tracker.record_request(1, t0);

        // After 20 half-lives: heat ≈ 1 / 2^20 ≈ 0.000001.
        let t_late = t0 + Duration::from_secs(20);
        let heat = tracker.heat(1, t_late);
        assert!(heat < COLD_THRESHOLD, "heat should be near zero: {heat}");
        assert!(!tracker.is_hot(1, t_late));
    }

    // ── Hottest pieces ──────────────────────────────────────────────

    /// Hottest pieces sorted by descending heat.
    #[test]
    fn hottest_pieces_sorted() {
        let mut tracker = DemandTracker::new();
        let now = Instant::now();
        tracker.record_request(1, now);
        tracker.record_request(2, now);
        tracker.record_request(2, now);
        tracker.record_request(3, now);
        tracker.record_request(3, now);
        tracker.record_request(3, now);

        let hot = tracker.hottest_pieces(3, now);
        assert_eq!(hot.len(), 3);
        assert_eq!(hot[0].0, 3); // 3 requests
        assert_eq!(hot[1].0, 2); // 2 requests
        assert_eq!(hot[2].0, 1); // 1 request
    }

    /// Hottest pieces respects the limit N.
    #[test]
    fn hottest_pieces_limit() {
        let mut tracker = DemandTracker::new();
        let now = Instant::now();
        for i in 0..10 {
            tracker.record_request(i, now);
        }
        let hot = tracker.hottest_pieces(3, now);
        assert_eq!(hot.len(), 3);
    }

    /// Hottest pieces excludes cold entries.
    #[test]
    fn hottest_pieces_excludes_cold() {
        let mut tracker = DemandTracker::with_half_life(1.0);
        let t0 = Instant::now();
        tracker.record_request(1, t0);

        let t_late = t0 + Duration::from_secs(30);
        let hot = tracker.hottest_pieces(10, t_late);
        assert!(hot.is_empty(), "cold piece should be excluded");
    }

    // ── Eviction ────────────────────────────────────────────────────

    /// Evict cold removes entries below threshold.
    #[test]
    fn evict_cold_removes_old() {
        let mut tracker = DemandTracker::with_half_life(1.0);
        let t0 = Instant::now();
        tracker.record_request(1, t0);
        tracker.record_request(2, t0);
        assert_eq!(tracker.tracked_count(), 2);

        let t_late = t0 + Duration::from_secs(30);
        tracker.evict_cold(t_late);
        assert_eq!(tracker.tracked_count(), 0);
    }

    /// Evict cold keeps hot entries.
    #[test]
    fn evict_cold_keeps_hot() {
        let mut tracker = DemandTracker::with_half_life(60.0);
        let t0 = Instant::now();
        tracker.record_request(1, t0);

        let t1 = t0 + Duration::from_secs(5);
        tracker.evict_cold(t1);
        assert_eq!(tracker.tracked_count(), 1);
    }

    // ── Configuration ───────────────────────────────────────────────

    /// Custom half-life is stored correctly.
    #[test]
    fn custom_half_life() {
        let tracker = DemandTracker::with_half_life(120.0);
        assert!((tracker.decay_half_life_secs() - 120.0).abs() < f64::EPSILON);
    }

    /// Very small half-life is clamped to 0.1.
    #[test]
    fn half_life_clamped() {
        let tracker = DemandTracker::with_half_life(-5.0);
        assert!((tracker.decay_half_life_secs() - 0.1).abs() < f64::EPSILON);
    }

    /// Default trait works.
    #[test]
    fn tracker_default() {
        let tracker = DemandTracker::default();
        assert_eq!(tracker.tracked_count(), 0);
    }

    /// Debug output includes tracked count.
    #[test]
    fn tracker_debug() {
        let tracker = DemandTracker::new();
        let debug = format!("{tracker:?}");
        assert!(debug.contains("tracked_count"), "debug: {debug}");
    }
}
