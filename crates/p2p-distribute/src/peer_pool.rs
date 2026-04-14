// SPDX-License-Identifier: MIT OR Apache-2.0

//! Peer lifecycle pool — connection limits, eviction, reconnection
//! backoff, and composite scoring for peer selection.
//!
//! ## What
//!
//! [`PeerPool`] manages a bounded set of active peers.  It enforces
//! connection limits, evicts underperforming peers, tracks reconnection
//! backoff with exponential delay, and ranks peers by composite score
//! (speed × reliability × credit) for download scheduling.
//!
//! ## Why — libtorrent / aria2 peer management lesson
//!
//! Production P2P clients maintain a peer pool with strict lifecycle
//! management:
//!
//! - **Connection limits** — too many connections waste OS resources
//!   (file descriptors, memory) and increase context-switch overhead.
//!   aria2 defaults to 55 global connections.
//! - **Eviction of slow/bad peers** — peers that consistently fail,
//!   send corrupt data, or transfer slowly should be replaced with
//!   fresh peers from the DHT/tracker/PEX.
//! - **Reconnection backoff** — after a disconnect, don't immediately
//!   reconnect.  Use exponential backoff to avoid hammering peers that
//!   are temporarily down (RFC 6298 pattern).
//! - **Credit-weighted scoring** — peers that upload more to us get
//!   priority reconnection and more download slots (eMule credit
//!   system integration).
//!
//! ## How
//!
//! 1. `PeerPool::new(config)` — set connection limits and thresholds.
//! 2. `try_add(peer_id, now)` — attempt to add a peer (respects limits
//!    and backoff).
//! 3. `evict_worst(now)` — remove the lowest-scoring active peer.
//! 4. `record_disconnect(peer_id, now)` — mark peer as disconnected,
//!    start backoff timer.
//! 5. `ranked_peers(now)` — get active peers sorted by composite score.

use std::time::{Duration, Instant};

// ── Constants ───────────────────────────────────────────────────────

/// Default maximum number of active peers.
const DEFAULT_MAX_PEERS: usize = 55;

/// Initial reconnection backoff duration.
const INITIAL_BACKOFF: Duration = Duration::from_secs(5);

/// Maximum reconnection backoff duration (capped exponential).
const MAX_BACKOFF: Duration = Duration::from_secs(300);

/// Backoff multiplier per consecutive failure.
const BACKOFF_MULTIPLIER: u32 = 2;

/// Score below which a peer is considered for eviction.
/// Peers scoring under this threshold when the pool is full get evicted
/// in favor of new candidates.
const EVICTION_SCORE_THRESHOLD: u64 = 100;

/// Minimum time a peer must be active before it can be evicted.
/// Prevents churn from evicting peers before we've measured them.
const MIN_ACTIVE_DURATION: Duration = Duration::from_secs(30);

// ── PeerPoolConfig ──────────────────────────────────────────────────

/// Configuration for a peer pool.
#[derive(Debug, Clone)]
pub struct PeerPoolConfig {
    /// Maximum number of simultaneously active peers.
    pub max_peers: usize,
    /// Minimum score to avoid eviction when pool is full.
    pub eviction_threshold: u64,
    /// Minimum time a peer must be active before eviction is considered.
    pub min_active_duration: Duration,
}

impl Default for PeerPoolConfig {
    fn default() -> Self {
        Self {
            max_peers: DEFAULT_MAX_PEERS,
            eviction_threshold: EVICTION_SCORE_THRESHOLD,
            min_active_duration: MIN_ACTIVE_DURATION,
        }
    }
}

// ── PeerEntry ───────────────────────────────────────────────────────

/// Lifecycle state of a single peer in the pool.
#[derive(Debug, Clone)]
pub struct PeerEntry {
    /// Unique peer identifier (opaque, matches PeerId bytes).
    peer_id: [u8; 32],
    /// Current state in the lifecycle.
    state: PeerState,
    /// Cumulative composite score (updated externally).
    score: u64,
    /// Number of consecutive disconnections (drives backoff).
    disconnect_count: u32,
    /// When the peer entered the current state.
    state_changed_at: Instant,
    /// When reconnection is allowed (only relevant in `Disconnected`).
    reconnect_after: Option<Instant>,
}

/// Lifecycle state of a peer.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PeerState {
    /// Peer is actively connected and exchanging data.
    Active,
    /// Peer disconnected; waiting for backoff to expire.
    Disconnected,
    /// Peer is permanently banned (corruption, protocol violation).
    Banned,
}

impl PeerEntry {
    /// Creates a new active peer entry.
    fn new_active(peer_id: [u8; 32], now: Instant) -> Self {
        Self {
            peer_id,
            state: PeerState::Active,
            score: 0,
            disconnect_count: 0,
            state_changed_at: now,
            reconnect_after: None,
        }
    }

    /// Returns the peer identifier.
    pub fn peer_id(&self) -> &[u8; 32] {
        &self.peer_id
    }

    /// Returns the current lifecycle state.
    pub fn state(&self) -> PeerState {
        self.state
    }

    /// Returns the current composite score.
    pub fn score(&self) -> u64 {
        self.score
    }

    /// Returns the consecutive disconnect count.
    pub fn disconnect_count(&self) -> u32 {
        self.disconnect_count
    }

    /// Returns how long the peer has been in its current state.
    pub fn time_in_state(&self, now: Instant) -> Duration {
        now.duration_since(self.state_changed_at)
    }

    /// Returns whether the peer can be reconnected (backoff expired).
    pub fn can_reconnect(&self, now: Instant) -> bool {
        match self.state {
            PeerState::Disconnected => self
                .reconnect_after
                .map(|deadline| now >= deadline)
                .unwrap_or(true),
            PeerState::Active => false, // Already connected.
            PeerState::Banned => false, // Never reconnect.
        }
    }
}

// ── PeerPool ────────────────────────────────────────────────────────

/// Bounded peer lifecycle manager with eviction and reconnection backoff.
#[derive(Debug)]
pub struct PeerPool {
    config: PeerPoolConfig,
    entries: Vec<PeerEntry>,
}

impl PeerPool {
    /// Creates an empty peer pool with the given configuration.
    pub fn new(config: PeerPoolConfig) -> Self {
        Self {
            config,
            entries: Vec::new(),
        }
    }

    /// Creates a pool with default configuration.
    pub fn with_defaults() -> Self {
        Self::new(PeerPoolConfig::default())
    }

    /// Attempts to add a peer as active.
    ///
    /// Returns `Ok(())` if the peer was added.
    /// Returns `Err(PoolFullError)` if the pool is at capacity and no
    /// peer qualifies for eviction.
    ///
    /// If the peer was previously disconnected, it re-enters as active
    /// only if the backoff period has expired.
    pub fn try_add(&mut self, peer_id: [u8; 32], now: Instant) -> Result<(), PoolFullError> {
        // Check if peer already exists.
        if let Some(entry) = self.entries.iter_mut().find(|e| e.peer_id == peer_id) {
            match entry.state {
                PeerState::Banned => return Err(PoolFullError::Banned),
                PeerState::Active => return Ok(()), // Already active — no-op.
                PeerState::Disconnected => {
                    if !entry.can_reconnect(now) {
                        return Err(PoolFullError::BackoffActive);
                    }
                    // Reconnect: reset to active state.
                    entry.state = PeerState::Active;
                    entry.state_changed_at = now;
                    return Ok(());
                }
            }
        }

        // New peer — check capacity.
        let active_count = self.active_count();
        if active_count >= self.config.max_peers {
            return Err(PoolFullError::AtCapacity {
                max_peers: self.config.max_peers,
            });
        }

        self.entries.push(PeerEntry::new_active(peer_id, now));
        Ok(())
    }

    /// Records a peer disconnection, starting the backoff timer.
    ///
    /// The backoff duration doubles with each consecutive disconnect:
    /// 5s → 10s → 20s → 40s → … → 300s (capped).
    pub fn record_disconnect(&mut self, peer_id: &[u8; 32], now: Instant) {
        if let Some(entry) = self.entries.iter_mut().find(|e| &e.peer_id == peer_id) {
            if entry.state == PeerState::Banned {
                return; // Banned peers stay banned.
            }
            entry.disconnect_count = entry.disconnect_count.saturating_add(1);
            entry.state = PeerState::Disconnected;
            entry.state_changed_at = now;

            // Exponential backoff: INITIAL_BACKOFF × 2^(n-1), capped at MAX_BACKOFF.
            let multiplier =
                BACKOFF_MULTIPLIER.saturating_pow(entry.disconnect_count.saturating_sub(1));
            let backoff = INITIAL_BACKOFF.saturating_mul(multiplier).min(MAX_BACKOFF);
            entry.reconnect_after = Some(now + backoff);
        }
    }

    /// Permanently bans a peer (e.g. for corruption or protocol violation).
    pub fn ban(&mut self, peer_id: &[u8; 32], now: Instant) {
        if let Some(entry) = self.entries.iter_mut().find(|e| &e.peer_id == peer_id) {
            entry.state = PeerState::Banned;
            entry.state_changed_at = now;
            entry.reconnect_after = None;
        }
    }

    /// Updates the composite score for a peer.
    pub fn update_score(&mut self, peer_id: &[u8; 32], score: u64) {
        if let Some(entry) = self.entries.iter_mut().find(|e| &e.peer_id == peer_id) {
            entry.score = score;
        }
    }

    /// Evicts the worst-scoring active peer that has been active long
    /// enough (past `min_active_duration`).
    ///
    /// Returns the evicted peer's ID, or `None` if no peer qualifies.
    pub fn evict_worst(&mut self, now: Instant) -> Option<[u8; 32]> {
        let min_active = self.config.min_active_duration;
        let threshold = self.config.eviction_threshold;

        // Find the lowest-scoring active peer that's old enough.
        let candidate = self
            .entries
            .iter()
            .enumerate()
            .filter(|(_, e)| {
                e.state == PeerState::Active
                    && e.time_in_state(now) >= min_active
                    && e.score < threshold
            })
            .min_by_key(|(_, e)| e.score)
            .map(|(i, _)| i);

        if let Some(idx) = candidate {
            let entry = self.entries.remove(idx);
            Some(entry.peer_id)
        } else {
            None
        }
    }

    /// Returns active peers sorted by score (highest first).
    pub fn ranked_peers(&self) -> Vec<&PeerEntry> {
        let mut active: Vec<&PeerEntry> = self
            .entries
            .iter()
            .filter(|e| e.state == PeerState::Active)
            .collect();
        active.sort_by(|a, b| b.score.cmp(&a.score));
        active
    }

    /// Returns peers that are disconnected and ready for reconnection.
    pub fn reconnectable_peers(&self, now: Instant) -> Vec<&PeerEntry> {
        self.entries
            .iter()
            .filter(|e| e.state == PeerState::Disconnected && e.can_reconnect(now))
            .collect()
    }

    /// Number of currently active (connected) peers.
    pub fn active_count(&self) -> usize {
        self.entries
            .iter()
            .filter(|e| e.state == PeerState::Active)
            .count()
    }

    /// Total entries tracked (active + disconnected + banned).
    pub fn total_count(&self) -> usize {
        self.entries.len()
    }

    /// Maximum number of active peers allowed.
    pub fn max_peers(&self) -> usize {
        self.config.max_peers
    }

    /// Returns `true` if the pool has room for more active peers.
    pub fn has_capacity(&self) -> bool {
        self.active_count() < self.config.max_peers
    }

    /// Removes all disconnected entries whose backoff has expired and
    /// who were not seen for a long time.  Frees memory for stale peers.
    pub fn prune_stale(&mut self, stale_threshold: Duration, now: Instant) {
        self.entries.retain(|e| {
            if e.state == PeerState::Disconnected {
                e.time_in_state(now) < stale_threshold
            } else {
                true // Keep active and banned entries.
            }
        });
    }

    /// Returns the entry for a specific peer, if it exists.
    pub fn get(&self, peer_id: &[u8; 32]) -> Option<&PeerEntry> {
        self.entries.iter().find(|e| &e.peer_id == peer_id)
    }
}

// ── Errors ──────────────────────────────────────────────────────────

/// Error returned when a peer cannot be added to the pool.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum PoolFullError {
    /// Pool is at maximum active peer capacity.
    #[error("pool at capacity ({max_peers} active peers)")]
    AtCapacity {
        /// The configured maximum.
        max_peers: usize,
    },
    /// Peer is currently in backoff after a disconnect.
    #[error("peer is in reconnection backoff")]
    BackoffActive,
    /// Peer is permanently banned.
    #[error("peer is banned")]
    Banned,
}

// ── Tests ───────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;

    /// Helper: creates a peer ID from a single byte (zero-padded).
    fn pid(b: u8) -> [u8; 32] {
        let mut id = [0u8; 32];
        id[0] = b;
        id
    }

    // ── Basic operations ────────────────────────────────────────────

    /// Adding a peer increases active count.
    ///
    /// `try_add` on an empty pool must succeed and increment
    /// `active_count`.
    #[test]
    fn add_peer_increments_count() {
        let now = Instant::now();
        let mut pool = PeerPool::with_defaults();
        pool.try_add(pid(1), now).unwrap();
        assert_eq!(pool.active_count(), 1);
        assert_eq!(pool.total_count(), 1);
    }

    /// Adding the same peer twice is idempotent.
    ///
    /// If a peer is already active, re-adding should not create a
    /// duplicate entry.
    #[test]
    fn add_same_peer_idempotent() {
        let now = Instant::now();
        let mut pool = PeerPool::with_defaults();
        pool.try_add(pid(1), now).unwrap();
        pool.try_add(pid(1), now).unwrap();
        assert_eq!(pool.active_count(), 1);
    }

    /// Pool respects max_peers capacity.
    ///
    /// Adding beyond the limit returns `AtCapacity`.
    #[test]
    fn pool_capacity_enforced() {
        let now = Instant::now();
        let config = PeerPoolConfig {
            max_peers: 2,
            ..Default::default()
        };
        let mut pool = PeerPool::new(config);
        pool.try_add(pid(1), now).unwrap();
        pool.try_add(pid(2), now).unwrap();

        let err = pool.try_add(pid(3), now).unwrap_err();
        assert!(matches!(err, PoolFullError::AtCapacity { max_peers: 2 }));
    }

    // ── Disconnect and backoff ──────────────────────────────────────

    /// Disconnection transitions peer to Disconnected state.
    ///
    /// After `record_disconnect`, the peer should be in Disconnected
    /// state and the active count should decrease.
    #[test]
    fn disconnect_changes_state() {
        let now = Instant::now();
        let mut pool = PeerPool::with_defaults();
        pool.try_add(pid(1), now).unwrap();
        pool.record_disconnect(&pid(1), now);

        let entry = pool.get(&pid(1)).unwrap();
        assert_eq!(entry.state(), PeerState::Disconnected);
        assert_eq!(pool.active_count(), 0);
    }

    /// Backoff prevents immediate reconnection.
    ///
    /// After disconnect, `try_add` must fail with `BackoffActive`
    /// until the backoff period expires.
    #[test]
    fn backoff_prevents_reconnect() {
        let now = Instant::now();
        let mut pool = PeerPool::with_defaults();
        pool.try_add(pid(1), now).unwrap();
        pool.record_disconnect(&pid(1), now);

        // Immediate reconnect attempt fails.
        let err = pool.try_add(pid(1), now).unwrap_err();
        assert!(matches!(err, PoolFullError::BackoffActive));
    }

    /// Reconnection succeeds after backoff expires.
    ///
    /// After waiting INITIAL_BACKOFF (5s), the peer should be
    /// reconnectable.
    #[test]
    fn reconnect_after_backoff() {
        let t0 = Instant::now();
        let mut pool = PeerPool::with_defaults();
        pool.try_add(pid(1), t0).unwrap();
        pool.record_disconnect(&pid(1), t0);

        // After backoff.
        let t1 = t0 + INITIAL_BACKOFF + Duration::from_millis(1);
        pool.try_add(pid(1), t1).unwrap();
        assert_eq!(pool.active_count(), 1);
    }

    /// Exponential backoff doubles each disconnect.
    ///
    /// First disconnect: 5s.  Second: 10s.  Third: 20s.
    #[test]
    fn exponential_backoff() {
        let t0 = Instant::now();
        let mut pool = PeerPool::with_defaults();
        pool.try_add(pid(1), t0).unwrap();

        // First disconnect → 5s backoff.
        pool.record_disconnect(&pid(1), t0);
        let t1 = t0 + Duration::from_secs(6);
        pool.try_add(pid(1), t1).unwrap();

        // Second disconnect → 10s backoff.
        pool.record_disconnect(&pid(1), t1);
        let t2 = t1 + Duration::from_secs(8);
        let err = pool.try_add(pid(1), t2).unwrap_err();
        assert!(matches!(err, PoolFullError::BackoffActive));

        // Wait long enough for 10s backoff.
        let t3 = t1 + Duration::from_secs(11);
        pool.try_add(pid(1), t3).unwrap();
    }

    /// Backoff is capped at MAX_BACKOFF.
    ///
    /// Even after many disconnects, backoff never exceeds 300 seconds.
    #[test]
    fn backoff_capped() {
        let t0 = Instant::now();
        let mut pool = PeerPool::with_defaults();
        pool.try_add(pid(1), t0).unwrap();

        // Disconnect many times.
        let mut t = t0;
        for _ in 0..20 {
            pool.record_disconnect(&pid(1), t);
            t += MAX_BACKOFF + Duration::from_secs(1);
            pool.try_add(pid(1), t).unwrap();
        }

        // After 20 disconnects, backoff should still be ≤ MAX_BACKOFF.
        pool.record_disconnect(&pid(1), t);
        let entry = pool.get(&pid(1)).unwrap();
        let deadline = entry.reconnect_after.unwrap();
        let actual_backoff = deadline.duration_since(t);
        assert!(actual_backoff <= MAX_BACKOFF);
    }

    // ── Banning ─────────────────────────────────────────────────────

    /// Banned peers cannot be re-added.
    ///
    /// Ban is permanent — `try_add` returns `Banned`.
    #[test]
    fn ban_prevents_reconnect() {
        let now = Instant::now();
        let mut pool = PeerPool::with_defaults();
        pool.try_add(pid(1), now).unwrap();
        pool.ban(&pid(1), now);

        let err = pool.try_add(pid(1), now).unwrap_err();
        assert!(matches!(err, PoolFullError::Banned));
    }

    /// Ban overrides disconnected state.
    #[test]
    fn ban_from_disconnected() {
        let now = Instant::now();
        let mut pool = PeerPool::with_defaults();
        pool.try_add(pid(1), now).unwrap();
        pool.record_disconnect(&pid(1), now);
        pool.ban(&pid(1), now);

        let entry = pool.get(&pid(1)).unwrap();
        assert_eq!(entry.state(), PeerState::Banned);
    }

    // ── Scoring and eviction ────────────────────────────────────────

    /// `ranked_peers` returns active peers sorted by score descending.
    ///
    /// Higher-scoring peers should appear first for download scheduling.
    #[test]
    fn ranked_peers_sorted() {
        let now = Instant::now();
        let mut pool = PeerPool::with_defaults();
        pool.try_add(pid(1), now).unwrap();
        pool.try_add(pid(2), now).unwrap();
        pool.try_add(pid(3), now).unwrap();

        pool.update_score(&pid(1), 100);
        pool.update_score(&pid(2), 500);
        pool.update_score(&pid(3), 250);

        let ranked = pool.ranked_peers();
        assert_eq!(ranked.len(), 3);
        assert_eq!(ranked[0].peer_id[0], 2); // score 500
        assert_eq!(ranked[1].peer_id[0], 3); // score 250
        assert_eq!(ranked[2].peer_id[0], 1); // score 100
    }

    /// `evict_worst` removes the lowest-scoring active peer.
    ///
    /// Only peers below the eviction threshold and past the minimum
    /// active duration are eligible.
    #[test]
    fn evict_worst_removes_lowest() {
        let t0 = Instant::now();
        let config = PeerPoolConfig {
            max_peers: 10,
            eviction_threshold: 200,
            min_active_duration: Duration::from_secs(0), // Allow immediate eviction for test.
        };
        let mut pool = PeerPool::new(config);
        pool.try_add(pid(1), t0).unwrap();
        pool.try_add(pid(2), t0).unwrap();

        pool.update_score(&pid(1), 50); // Below threshold → evictable.
        pool.update_score(&pid(2), 300); // Above threshold → safe.

        let evicted = pool.evict_worst(t0);
        assert_eq!(evicted, Some(pid(1)));
        assert_eq!(pool.active_count(), 1);
    }

    /// `evict_worst` respects min_active_duration.
    ///
    /// A peer that joined too recently cannot be evicted, even with a
    /// low score.
    #[test]
    fn evict_respects_min_active_duration() {
        let now = Instant::now();
        let config = PeerPoolConfig {
            max_peers: 10,
            eviction_threshold: 200,
            min_active_duration: Duration::from_secs(60),
        };
        let mut pool = PeerPool::new(config);
        pool.try_add(pid(1), now).unwrap();
        pool.update_score(&pid(1), 10);

        // Too soon — min_active_duration not met.
        assert_eq!(pool.evict_worst(now), None);

        // After 60 seconds — eligible.
        let later = now + Duration::from_secs(61);
        assert_eq!(pool.evict_worst(later), Some(pid(1)));
    }

    /// `evict_worst` returns None when no peers qualify.
    #[test]
    fn evict_worst_none_when_all_above_threshold() {
        let now = Instant::now();
        let config = PeerPoolConfig {
            max_peers: 10,
            eviction_threshold: 100,
            min_active_duration: Duration::from_secs(0),
        };
        let mut pool = PeerPool::new(config);
        pool.try_add(pid(1), now).unwrap();
        pool.update_score(&pid(1), 500);

        assert_eq!(pool.evict_worst(now), None);
    }

    // ── Reconnectable peers ─────────────────────────────────────────

    /// `reconnectable_peers` lists disconnected peers past backoff.
    #[test]
    fn reconnectable_peers_lists_ready() {
        let t0 = Instant::now();
        let mut pool = PeerPool::with_defaults();
        pool.try_add(pid(1), t0).unwrap();
        pool.try_add(pid(2), t0).unwrap();

        pool.record_disconnect(&pid(1), t0);
        pool.record_disconnect(&pid(2), t0);

        // At t0 — neither is ready.
        assert_eq!(pool.reconnectable_peers(t0).len(), 0);

        // After backoff — both ready.
        let later = t0 + INITIAL_BACKOFF + Duration::from_secs(1);
        assert_eq!(pool.reconnectable_peers(later).len(), 2);
    }

    // ── Pruning ─────────────────────────────────────────────────────

    /// `prune_stale` removes old disconnected entries.
    ///
    /// Active and banned entries are kept regardless of age.
    #[test]
    fn prune_stale_removes_old_disconnected() {
        let t0 = Instant::now();
        let mut pool = PeerPool::with_defaults();
        pool.try_add(pid(1), t0).unwrap();
        pool.try_add(pid(2), t0).unwrap();
        pool.try_add(pid(3), t0).unwrap();

        pool.record_disconnect(&pid(1), t0); // Will be stale.
        pool.ban(&pid(2), t0); // Banned — kept.
                               // pid(3) stays active.

        let later = t0 + Duration::from_secs(3600);
        pool.prune_stale(Duration::from_secs(600), later);

        assert_eq!(pool.total_count(), 2); // pid(2) banned + pid(3) active.
        assert!(pool.get(&pid(1)).is_none());
        assert!(pool.get(&pid(2)).is_some());
        assert!(pool.get(&pid(3)).is_some());
    }

    // ── Edge cases ──────────────────────────────────────────────────

    /// Empty pool has zero counts and has_capacity.
    #[test]
    fn empty_pool() {
        let pool = PeerPool::with_defaults();
        assert_eq!(pool.active_count(), 0);
        assert_eq!(pool.total_count(), 0);
        assert!(pool.has_capacity());
        assert_eq!(pool.max_peers(), DEFAULT_MAX_PEERS);
    }

    /// `get` returns None for unknown peer.
    #[test]
    fn get_unknown_peer() {
        let pool = PeerPool::with_defaults();
        assert!(pool.get(&pid(99)).is_none());
    }

    /// `ranked_peers` excludes disconnected and banned peers.
    #[test]
    fn ranked_excludes_non_active() {
        let now = Instant::now();
        let mut pool = PeerPool::with_defaults();
        pool.try_add(pid(1), now).unwrap();
        pool.try_add(pid(2), now).unwrap();
        pool.try_add(pid(3), now).unwrap();

        pool.record_disconnect(&pid(2), now);
        pool.ban(&pid(3), now);

        let ranked = pool.ranked_peers();
        assert_eq!(ranked.len(), 1);
        assert_eq!(ranked[0].peer_id[0], 1);
    }

    // ── Error Display ───────────────────────────────────────────────

    /// Error Display messages contain diagnostic context.
    #[test]
    fn error_display_messages() {
        let at_cap = PoolFullError::AtCapacity { max_peers: 55 };
        assert!(at_cap.to_string().contains("55"));

        let backoff = PoolFullError::BackoffActive;
        assert!(backoff.to_string().contains("backoff"));

        let banned = PoolFullError::Banned;
        assert!(banned.to_string().contains("banned"));
    }

    /// Debug formatting works.
    #[test]
    fn debug_format() {
        let now = Instant::now();
        let mut pool = PeerPool::with_defaults();
        pool.try_add(pid(1), now).unwrap();
        let dbg = format!("{pool:?}");
        assert!(dbg.contains("PeerPool"));

        let entry = pool.get(&pid(1)).unwrap();
        let entry_dbg = format!("{entry:?}");
        assert!(entry_dbg.contains("PeerEntry"));
    }

    /// `disconnect_count` increments with each disconnect.
    #[test]
    fn disconnect_count_increments() {
        let t0 = Instant::now();
        let mut pool = PeerPool::with_defaults();
        pool.try_add(pid(1), t0).unwrap();

        pool.record_disconnect(&pid(1), t0);
        assert_eq!(pool.get(&pid(1)).unwrap().disconnect_count(), 1);

        let t1 = t0 + Duration::from_secs(10);
        pool.try_add(pid(1), t1).unwrap();
        pool.record_disconnect(&pid(1), t1);
        assert_eq!(pool.get(&pid(1)).unwrap().disconnect_count(), 2);
    }

    /// `time_in_state` reflects actual elapsed duration.
    #[test]
    fn time_in_state_accurate() {
        let t0 = Instant::now();
        let mut pool = PeerPool::with_defaults();
        pool.try_add(pid(1), t0).unwrap();

        let later = t0 + Duration::from_secs(42);
        let entry = pool.get(&pid(1)).unwrap();
        let elapsed = entry.time_in_state(later);
        assert_eq!(elapsed, Duration::from_secs(42));
    }
}
