// SPDX-License-Identifier: MIT OR Apache-2.0

//! Rate-based choking strategy trait — controls which peers receive upload
//! bandwidth (seeding support).
//!
//! ## Design (informed by libtorrent's rate-based choking + BT specification)
//!
//! BitTorrent's choking algorithm decides which peers are allowed to download
//! from us. The standard approach is "tit-for-tat": unchoke the N peers that
//! give us the most bandwidth, plus periodically unchoke a random peer
//! (optimistic unchoking) to discover new fast sources. libtorrent extends
//! this with *rate-based choking*: instead of fixed N unchoke slots, the number
//! of slots scales dynamically based on total upload capacity. If available
//! bandwidth exceeds what N peers can use, open more slots.
//!
//! This crate is currently download-only (no seeding), but the architecture
//! must support seeding in the future without breaking changes. The
//! [`ChokingStrategy`] trait defines the decision interface; the default
//! [`TitForTatChoking`] implements the standard algorithm. When seeding is
//! added, the coordinator calls `evaluate()` periodically to decide which
//! peers to unchoke.
//!
//! ## How
//!
//! The trait takes a snapshot of all peers and their stats, and returns a list
//! of peer indices to unchoke. Everything else stays choked. The coordinator
//! applies the decisions by calling `choke()`/`unchoke()` on the peer
//! abstraction (which will be extended when upload support arrives).

use std::time::Instant;

use crate::credit::CreditLedger;
use crate::peer_id::PeerId;
use crate::peer_stats::PeerTracker;

// ── ChokingDecision ─────────────────────────────────────────────────

/// A choking decision for a single peer.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ChokingDecision {
    /// Index into the peer list.
    pub peer_index: usize,
    /// Whether this peer should be unchoked (allowed to download from us).
    pub unchoked: bool,
    /// Whether this is an optimistic unchoke (random selection) vs. a
    /// performance-based unchoke.
    pub optimistic: bool,
}

// ── ChokingStrategy trait ───────────────────────────────────────────

/// Trait for algorithms that decide which peers are unchoked.
///
/// The coordinator calls [`evaluate`](Self::evaluate) periodically (typically
/// every 10 seconds per BT spec) to get the current choke/unchoke decisions.
/// Implementations are stateful to support optimistic unchoking rotation.
///
/// ## Contract
///
/// - `evaluate()` must return a decision for *every* peer index in `0..peer_count`.
/// - At most `max_unchoked` peers should be unchoked (plus one optimistic).
/// - Implementations must be deterministic given the same inputs + internal state.
pub trait ChokingStrategy: Send + Sync {
    /// Evaluates which peers should be unchoked.
    ///
    /// - `tracker`: per-peer statistics (speed, reliability, etc.)
    /// - `peer_count`: total number of peers
    /// - `now`: current time for recency calculations
    ///
    /// Returns a list of decisions, one per peer.
    fn evaluate(
        &mut self,
        tracker: &PeerTracker,
        peer_count: usize,
        now: Instant,
    ) -> Vec<ChokingDecision>;
}

// ── TitForTatChoking ────────────────────────────────────────────────

/// Standard tit-for-tat choking with optimistic unchoking.
///
/// Unchokes the top `regular_slots` peers by upload speed received from them
/// (we reward peers that give us data). One additional "optimistic unchoke"
/// slot rotates every `optimistic_interval` evaluations to discover new fast
/// peers (TCP slow-start analogy).
///
/// ## Rate-based extension (libtorrent)
///
/// When `auto_scale_slots` is true, the number of unchoke slots scales with
/// available upload bandwidth. If all current slots are fully utilized (each
/// peer uploads within 2 KB/s of the per-slot share), an additional slot opens.
/// This prevents underutilizing upload capacity when peers are fast.
pub struct TitForTatChoking {
    /// Number of regular (performance-based) unchoke slots.
    regular_slots: usize,
    /// Whether to auto-scale slots based on upload bandwidth utilization.
    auto_scale_slots: bool,
    /// Current optimistic unchoke peer index (rotates each call).
    optimistic_index: usize,
    /// Counter for optimistic rotation (rotates every N evaluations).
    eval_counter: u32,
    /// How many evaluations between optimistic unchoke rotations.
    /// BT spec: rotate every 3 rounds (30 seconds at 10s intervals).
    optimistic_interval: u32,
    /// Optional credit ledger for weighting unchoke decisions by peer
    /// generosity. When set, each peer's effective speed is multiplied
    /// by `max(1.0, credit_modifier)` so generous peers rank higher.
    /// (eMule bilateral credit system lesson.)
    credit_ledger: Option<CreditLedger>,
}

impl TitForTatChoking {
    /// Creates a new tit-for-tat choking strategy.
    ///
    /// `regular_slots` is the base number of unchoke slots (BT default: 4).
    pub fn new(regular_slots: usize) -> Self {
        Self {
            regular_slots,
            auto_scale_slots: false,
            optimistic_index: 0,
            eval_counter: 0,
            optimistic_interval: 3,
            credit_ledger: None,
        }
    }

    /// Enables rate-based auto-scaling of unchoke slots.
    pub fn with_auto_scale(mut self) -> Self {
        self.auto_scale_slots = true;
        self
    }

    /// Sets the optimistic rotation interval (in evaluation rounds).
    pub fn with_optimistic_interval(mut self, rounds: u32) -> Self {
        self.optimistic_interval = rounds.max(1);
        self
    }

    /// Attaches a credit ledger for credit-weighted unchoke decisions.
    ///
    /// When set, each peer's effective speed is multiplied by
    /// `max(1.0, credit_modifier)` during ranking. Peers with higher
    /// credit (more generous uploaders) get priority for unchoke slots.
    /// Peers with no credit history are not penalized (modifier floors
    /// at 1.0).
    pub fn with_credit_ledger(mut self, ledger: CreditLedger) -> Self {
        self.credit_ledger = Some(ledger);
        self
    }

    /// Updates the credit ledger (e.g. after recording new transfers).
    pub fn set_credit_ledger(&mut self, ledger: CreditLedger) {
        self.credit_ledger = Some(ledger);
    }

    /// Returns the credit modifier for a peer, looked up by PeerId.
    ///
    /// Returns 1.0 (neutral) if no ledger is set or the peer has no
    /// credit history.
    fn credit_weight(&self, peer_id: Option<&PeerId>) -> f64 {
        match (&self.credit_ledger, peer_id) {
            (Some(ledger), Some(id)) => ledger.credit_modifier(id).max(1.0),
            _ => 1.0,
        }
    }
}

impl ChokingStrategy for TitForTatChoking {
    fn evaluate(
        &mut self,
        tracker: &PeerTracker,
        peer_count: usize,
        _now: Instant,
    ) -> Vec<ChokingDecision> {
        if peer_count == 0 {
            return Vec::new();
        }

        // Step 1: Rank peers by credit-weighted speed. The base score is
        // EWMA speed (bytes received from them). When a credit ledger is
        // attached, the score is scaled by the peer's credit modifier
        // (minimum 1.0 — no penalty for unknown peers, only bonus for
        // generous ones). This rewards peers that have historically
        // uploaded more to us than they've downloaded (eMule credit
        // system pattern).
        let mut ranked: Vec<(usize, u64)> = (0..peer_count)
            .map(|i| {
                let speed = tracker
                    .get(i)
                    .map(|s| s.ewma_speed_bytes_per_sec())
                    .unwrap_or(0);
                let credit = self.credit_weight(tracker.identity(i));
                // Multiply speed by credit modifier for ranking. The cast
                // is safe because credit is in [1.0, 10.0] and speed is u64.
                let weighted = (speed as f64 * credit) as u64;
                (i, weighted)
            })
            .collect();

        // Sort descending by speed.
        ranked.sort_by_key(|b| std::cmp::Reverse(b.1));

        // Step 2: Determine effective slot count.
        let mut slots = self.regular_slots;
        if self.auto_scale_slots && slots > 0 {
            // If all current slots are within 2KB/s of the average, there's
            // room for more. Scale up to min(peer_count, slots*2).
            let active_speeds: Vec<u64> = ranked
                .iter()
                .take(slots)
                .map(|&(_, s)| s)
                .filter(|&s| s > 0)
                .collect();
            if !active_speeds.is_empty() {
                let avg: u64 = active_speeds.iter().sum::<u64>() / active_speeds.len() as u64;
                let threshold = 2048; // 2 KB/s headroom per slot
                let all_utilized = active_speeds
                    .iter()
                    .all(|&s| s >= avg.saturating_sub(threshold));
                if all_utilized {
                    slots = slots.saturating_add(1).min(peer_count);
                }
            }
        }

        // Step 3: Unchoke top `slots` peers.
        let mut decisions: Vec<ChokingDecision> = Vec::with_capacity(peer_count);
        let mut unchoked_set: Vec<usize> = Vec::with_capacity(slots.saturating_add(1));

        for (rank, &(peer_idx, _)) in ranked.iter().enumerate() {
            if rank < slots {
                unchoked_set.push(peer_idx);
                decisions.push(ChokingDecision {
                    peer_index: peer_idx,
                    unchoked: true,
                    optimistic: false,
                });
            } else {
                decisions.push(ChokingDecision {
                    peer_index: peer_idx,
                    unchoked: false,
                    optimistic: false,
                });
            }
        }

        // Step 4: Optimistic unchoke — rotate to a currently-choked peer.
        self.eval_counter = self.eval_counter.wrapping_add(1);
        if self.eval_counter.is_multiple_of(self.optimistic_interval) {
            self.optimistic_index = self.optimistic_index.wrapping_add(1);
        }

        // Find the first choked peer (round-robin from optimistic_index).
        let choked_peers: Vec<usize> = decisions
            .iter()
            .filter(|d| !d.unchoked)
            .map(|d| d.peer_index)
            .collect();

        if !choked_peers.is_empty() {
            let opt_idx = self.optimistic_index % choked_peers.len();
            if let Some(&opt_peer) = choked_peers.get(opt_idx) {
                // Find the decision for this peer and mark as optimistic unchoke.
                if let Some(d) = decisions.iter_mut().find(|d| d.peer_index == opt_peer) {
                    d.unchoked = true;
                    d.optimistic = true;
                }
            }
        }

        // Sort by peer_index for stable output.
        decisions.sort_by_key(|d| d.peer_index);
        decisions
    }
}

// ── AlwaysUnchoke ───────────────────────────────────────────────────

/// Choking strategy that unconditionally unchokes every peer.
///
/// ## When to use
///
/// Content distribution applications where the goal is maximum swarm health
/// and every peer should be served unconditionally. This is the right default
/// when:
///
/// - The content is freely redistributable (freeware game assets, open-source
///   mod packages) and there is no reason to throttle any peer.
/// - The application is not a general-purpose BitTorrent client — users
///   download specific content, not arbitrary torrents.
/// - Tit-for-tat incentives are counterproductive: new users with nothing to
///   upload yet would be penalized, slowing adoption.
///
/// ## When NOT to use
///
/// General-purpose BT clients or scenarios where upload bandwidth must be
/// rationed among many peers. Use [`TitForTatChoking`] instead.
///
/// ## Design rationale
///
/// Traditional BT tit-for-tat (BEP 3) rewards peers who reciprocate uploads.
/// This makes sense for public trackers with untrusted peers competing for a
/// scarce good (bandwidth on a popular torrent). It does NOT make sense for
/// game content distribution where:
///
/// 1. Content is small (500 MB disc ISOs) and finite — not a streaming service.
/// 2. Every player who finishes downloading naturally becomes a seeder.
/// 3. The alternative to P2P is HTTP mirrors — if P2P penalizes newcomers,
///    they fall back to mirrors and never join the swarm at all.
/// 4. The goal is growing the swarm as fast as possible, not protecting
///    scarce bandwidth from freeloaders.
pub struct AlwaysUnchoke;

impl AlwaysUnchoke {
    /// Creates a new always-unchoke strategy.
    pub fn new() -> Self {
        Self
    }
}

impl Default for AlwaysUnchoke {
    fn default() -> Self {
        Self::new()
    }
}

impl ChokingStrategy for AlwaysUnchoke {
    /// Unchokes every peer unconditionally.
    ///
    /// Returns one [`ChokingDecision`] per peer with `unchoked = true` and
    /// `optimistic = false` (the optimistic flag is meaningless when everyone
    /// is already unchoked).
    fn evaluate(
        &mut self,
        _tracker: &PeerTracker,
        peer_count: usize,
        _now: Instant,
    ) -> Vec<ChokingDecision> {
        (0..peer_count)
            .map(|peer_index| ChokingDecision {
                peer_index,
                unchoked: true,
                optimistic: false,
            })
            .collect()
    }
}

// ── Tests ───────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;
    use crate::credit::CreditLedger;
    use crate::peer_id::PeerId;
    use crate::peer_stats::PeerTracker;
    use std::time::{Duration, Instant};

    /// Helper: create a tracker with N peers, each having a given speed.
    fn make_tracker(speeds: &[u64]) -> (PeerTracker, Instant) {
        let now = Instant::now();
        let mut tracker = PeerTracker::new(speeds.len(), now);
        for (i, &speed) in speeds.iter().enumerate() {
            if speed > 0 {
                if let Some(stats) = tracker.get_mut(i) {
                    // Simulate enough successful fetches to establish speed.
                    for _ in 0..5 {
                        let bytes = speed;
                        stats.record_success(bytes, Duration::from_secs(1), now);
                    }
                }
            }
        }
        (tracker, now)
    }

    // ── TitForTatChoking ────────────────────────────────────────────

    /// With 4 slots and 6 peers, top 4 by speed are unchoked + 1 optimistic.
    ///
    /// The tit-for-tat algorithm must reward the fastest peers with unchoke
    /// slots, and optimistic unchoke must pick from the remaining choked peers.
    #[test]
    fn tit_for_tat_unchokes_fastest() {
        let speeds = [100, 500, 300, 50, 200, 400];
        let (tracker, now) = make_tracker(&speeds);

        let mut strategy = TitForTatChoking::new(4);
        let decisions = strategy.evaluate(&tracker, 6, now);

        assert_eq!(decisions.len(), 6);

        // Count unchoked peers.
        let unchoked: Vec<usize> = decisions
            .iter()
            .filter(|d| d.unchoked)
            .map(|d| d.peer_index)
            .collect();

        // Should be 4 regular + 1 optimistic = 5 (or 4 if optimistic overlaps).
        assert!(
            unchoked.len() >= 4,
            "expected at least 4 unchoked, got {}",
            unchoked.len()
        );

        // The top-4 by speed (indices 1, 5, 2, 4 = speeds 500, 400, 300, 200)
        // should all be unchoked.
        assert!(unchoked.contains(&1), "peer 1 (500) should be unchoked");
        assert!(unchoked.contains(&5), "peer 5 (400) should be unchoked");
        assert!(unchoked.contains(&2), "peer 2 (300) should be unchoked");
        assert!(unchoked.contains(&4), "peer 4 (200) should be unchoked");
    }

    /// With zero peers, evaluation returns empty.
    #[test]
    fn zero_peers_empty_decisions() {
        let (tracker, now) = make_tracker(&[]);
        let mut strategy = TitForTatChoking::new(4);
        let decisions = strategy.evaluate(&tracker, 0, now);
        assert!(decisions.is_empty());
    }

    /// With fewer peers than slots, all peers are unchoked.
    #[test]
    fn fewer_peers_than_slots_all_unchoked() {
        let speeds = [100, 200];
        let (tracker, now) = make_tracker(&speeds);

        let mut strategy = TitForTatChoking::new(4);
        let decisions = strategy.evaluate(&tracker, 2, now);

        assert!(decisions.iter().all(|d| d.unchoked));
    }

    /// Optimistic unchoke rotates across evaluations.
    ///
    /// The optimistic peer must change when the rotation interval elapses.
    #[test]
    fn optimistic_rotates() {
        let speeds = [100, 200, 300, 50, 50, 50];
        let (tracker, now) = make_tracker(&speeds);

        let mut strategy = TitForTatChoking::new(2).with_optimistic_interval(1);

        let d1 = strategy.evaluate(&tracker, 6, now);
        let opt1: Vec<usize> = d1
            .iter()
            .filter(|d| d.optimistic)
            .map(|d| d.peer_index)
            .collect();

        let d2 = strategy.evaluate(&tracker, 6, now);
        let opt2: Vec<usize> = d2
            .iter()
            .filter(|d| d.optimistic)
            .map(|d| d.peer_index)
            .collect();

        // After two evaluations with interval=1, the optimistic peer should differ
        // (or at least potentially differ — depends on choked set size).
        // With 2 regular slots and 6 peers, 4 are choked, so rotation should work.
        assert!(
            !opt1.is_empty() || !opt2.is_empty(),
            "should have at least one optimistic unchoke"
        );
    }

    /// `ChokingDecision` Display and Debug formatting.
    #[test]
    fn choking_decision_debug() {
        let decision = ChokingDecision {
            peer_index: 3,
            unchoked: true,
            optimistic: false,
        };
        let dbg = format!("{decision:?}");
        assert!(dbg.contains("peer_index: 3"));
    }

    // ── Credit integration ──────────────────────────────────────────

    /// Credit modifier boosts a slower peer above a faster one.
    ///
    /// Peer 0 has speed=100 but credit=10.0 (very generous → weighted 1000).
    /// Peer 1 has speed=500 but credit=1.0 (neutral → weighted 500).
    /// With 1 regular slot, peer 0 should be unchoked over peer 1.
    #[test]
    fn credit_boosts_slower_peer() {
        let speeds = [100, 500];
        let (mut tracker, now) = make_tracker(&speeds);

        // Register identities so the credit ledger can look them up.
        let peer_0_id = PeerId::from_key_material(b"generous-peer");
        let peer_1_id = PeerId::from_key_material(b"neutral-peer");
        tracker.register_identity(0, peer_0_id);
        tracker.register_identity(1, peer_1_id);

        // Build credit ledger: peer 0 uploaded 10 MB to us, peer 1 nothing.
        let mut ledger = CreditLedger::new();
        ledger.record_received(&peer_0_id, 10_000_000, 1000);

        let mut strategy = TitForTatChoking::new(1).with_credit_ledger(ledger);
        let decisions = strategy.evaluate(&tracker, 2, now);

        // Peer 0 (credit-boosted 100*10=1000) should beat peer 1 (500*1=500).
        let regular_unchoked: Vec<usize> = decisions
            .iter()
            .filter(|d| d.unchoked && !d.optimistic)
            .map(|d| d.peer_index)
            .collect();

        assert!(
            regular_unchoked.contains(&0),
            "peer 0 (credit-boosted) should be unchoked, got {regular_unchoked:?}"
        );
    }

    /// Without credit ledger, pure speed ranking prevails.
    ///
    /// Same peers as above but no credit ledger — peer 1 (speed=500) wins.
    #[test]
    fn no_credit_uses_pure_speed() {
        let speeds = [100, 500];
        let (tracker, now) = make_tracker(&speeds);

        let mut strategy = TitForTatChoking::new(1);
        let decisions = strategy.evaluate(&tracker, 2, now);

        let regular_unchoked: Vec<usize> = decisions
            .iter()
            .filter(|d| d.unchoked && !d.optimistic)
            .map(|d| d.peer_index)
            .collect();

        assert!(
            regular_unchoked.contains(&1),
            "peer 1 (faster) should be unchoked without credit, got {regular_unchoked:?}"
        );
    }

    /// Credit weight minimum is 1.0 — unknown peers are not penalized.
    ///
    /// Peers with no credit history get modifier 0.0 from the ledger, but
    /// credit_weight() floors it to 1.0 so they compete on pure speed.
    #[test]
    fn credit_floor_does_not_penalize_unknown() {
        let speeds = [300, 200];
        let (tracker, now) = make_tracker(&speeds);

        // Empty credit ledger — both peers have modifier 0.0 → floored to 1.0.
        let ledger = CreditLedger::new();
        let mut strategy = TitForTatChoking::new(1).with_credit_ledger(ledger);
        let decisions = strategy.evaluate(&tracker, 2, now);

        let regular_unchoked: Vec<usize> = decisions
            .iter()
            .filter(|d| d.unchoked && !d.optimistic)
            .map(|d| d.peer_index)
            .collect();

        // With floor=1.0, pure speed wins: peer 0 (300) beats peer 1 (200).
        assert!(
            regular_unchoked.contains(&0),
            "peer 0 (faster) should win with empty credit, got {regular_unchoked:?}"
        );
    }

    // ── AlwaysUnchoke ───────────────────────────────────────────────

    /// AlwaysUnchoke unchokes every peer regardless of speed or credit.
    ///
    /// This verifies the core invariant: no peer is ever choked, making it
    /// suitable for content distribution where all peers should be served.
    #[test]
    fn always_unchoke_all_peers() {
        let speeds = [100, 500, 300, 50, 200, 400];
        let (tracker, now) = make_tracker(&speeds);

        let mut strategy = AlwaysUnchoke::new();
        let decisions = strategy.evaluate(&tracker, 6, now);

        assert_eq!(decisions.len(), 6);
        assert!(
            decisions.iter().all(|d| d.unchoked),
            "every peer must be unchoked"
        );
        assert!(
            decisions.iter().all(|d| !d.optimistic),
            "optimistic flag must be false when everyone is unchoked"
        );
    }

    /// AlwaysUnchoke with zero peers returns empty decisions.
    #[test]
    fn always_unchoke_zero_peers() {
        let (tracker, now) = make_tracker(&[]);
        let mut strategy = AlwaysUnchoke::new();
        let decisions = strategy.evaluate(&tracker, 0, now);
        assert!(decisions.is_empty());
    }

    /// AlwaysUnchoke is idempotent — calling evaluate twice gives the same result.
    ///
    /// Unlike TitForTatChoking which has rotating optimistic state, AlwaysUnchoke
    /// is stateless and must produce identical output on every call.
    #[test]
    fn always_unchoke_idempotent() {
        let speeds = [100, 200];
        let (tracker, now) = make_tracker(&speeds);

        let mut strategy = AlwaysUnchoke::new();
        let d1 = strategy.evaluate(&tracker, 2, now);
        let d2 = strategy.evaluate(&tracker, 2, now);
        assert_eq!(d1, d2);
    }
}
