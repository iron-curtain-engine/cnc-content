// SPDX-License-Identifier: MIT OR Apache-2.0

//! Connection budget — resource limits for peer connections (libp2p pattern).
//!
//! ## What
//!
//! A resource accounting type that enforces hard limits on concurrent peer
//! connections, preventing resource exhaustion and eclipse attacks.
//!
//! ## Why — libp2p `connection_limits` lesson
//!
//! libp2p's `ConnectionLimits` behaviour enforces six independent limits:
//! max pending incoming, max pending outgoing, max established incoming,
//! max established outgoing, max established total, and max per-peer.
//! Without these, a single attacker can open hundreds of connections,
//! crowding out legitimate peers (eclipse attack) or exhausting file
//! descriptors.
//!
//! Our model is simpler because we are download-only (no incoming
//! connections), but we still need:
//! - **Max total peers** — prevent file descriptor exhaustion.
//! - **Max peers per identity** — prevent one PeerId from monopolising slots.
//! - **Max pending** — prevent slow-handshake peers from blocking new ones.
//!
//! ## How
//!
//! `ConnectionBudget` is a pure accounting type with no I/O. The coordinator
//! calls `try_acquire()` before adding a peer and `release()` when removing
//! one. All operations are O(1).

/// Resource limits for peer connections.
///
/// ```
/// use p2p_distribute::ConnectionBudget;
///
/// let mut budget = ConnectionBudget::new(4, 2, 2);
/// assert!(budget.try_acquire_established("peer_a"));
/// assert!(budget.try_acquire_established("peer_a")); // 2nd slot
/// assert!(!budget.try_acquire_established("peer_a")); // per-peer limit hit
/// budget.release_established("peer_a");
/// assert!(budget.try_acquire_established("peer_a")); // slot freed
/// ```
#[derive(Debug)]
pub struct ConnectionBudget {
    /// Maximum total established connections across all peers.
    max_established: u32,
    /// Maximum established connections from a single peer identity.
    max_per_peer: u32,
    /// Maximum connections in the "pending" (handshake) state.
    max_pending: u32,
    /// Current number of established connections.
    current_established: u32,
    /// Current number of pending connections.
    current_pending: u32,
    /// Per-peer connection counts. Key is the peer's string identity
    /// (PeerId encoded form or address).
    per_peer_counts: std::collections::HashMap<String, u32>,
}

/// Error when a connection budget limit is exceeded.
///
/// Carries which limit was hit so the caller can decide whether to queue,
/// drop, or log the rejection.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum BudgetExceeded {
    /// Total established connections at maximum.
    TotalEstablished {
        /// Current limit.
        limit: u32,
    },
    /// This specific peer has too many connections.
    PerPeer {
        /// The peer that exceeded its allocation.
        peer: String,
        /// Current limit.
        limit: u32,
    },
    /// Too many connections in handshake state.
    TotalPending {
        /// Current limit.
        limit: u32,
    },
}

impl std::fmt::Display for BudgetExceeded {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::TotalEstablished { limit } => {
                write!(f, "total established connections at limit ({limit})")
            }
            Self::PerPeer { peer, limit } => {
                write!(f, "peer {peer} at per-peer limit ({limit})")
            }
            Self::TotalPending { limit } => {
                write!(f, "pending connections at limit ({limit})")
            }
        }
    }
}

impl ConnectionBudget {
    /// Creates a new budget with the given limits.
    ///
    /// ## Defaults for content downloading
    ///
    /// - `max_established`: 50 — matches BT client conventions
    ///   (libtorrent default: 50, qBittorrent default: 100).
    /// - `max_per_peer`: 1 — one connection per peer identity is the norm.
    /// - `max_pending`: 10 — prevents slow handshakes from blocking faster
    ///   peers.
    pub fn new(max_established: u32, max_pending: u32, max_per_peer: u32) -> Self {
        Self {
            max_established,
            max_per_peer,
            max_pending,
            current_established: 0,
            current_pending: 0,
            per_peer_counts: std::collections::HashMap::new(),
        }
    }

    /// Default budget suitable for content downloading.
    ///
    /// 50 established, 10 pending, 1 per peer.
    pub fn default_download() -> Self {
        Self::new(50, 10, 1)
    }

    /// Try to acquire an established connection slot.
    ///
    /// Returns `true` if the slot was granted, `false` if a limit would
    /// be exceeded.
    pub fn try_acquire_established(&mut self, peer_id: &str) -> bool {
        self.try_acquire_established_checked(peer_id).is_ok()
    }

    /// Try to acquire with detailed error on failure.
    pub fn try_acquire_established_checked(&mut self, peer_id: &str) -> Result<(), BudgetExceeded> {
        if self.current_established >= self.max_established {
            return Err(BudgetExceeded::TotalEstablished {
                limit: self.max_established,
            });
        }
        let peer_count = self.per_peer_counts.get(peer_id).copied().unwrap_or(0);
        if peer_count >= self.max_per_peer {
            return Err(BudgetExceeded::PerPeer {
                peer: peer_id.to_string(),
                limit: self.max_per_peer,
            });
        }
        self.current_established = self.current_established.saturating_add(1);
        *self.per_peer_counts.entry(peer_id.to_string()).or_insert(0) += 1;
        Ok(())
    }

    /// Release an established connection slot.
    ///
    /// No-op if the peer has no tracked connections (prevents underflow).
    pub fn release_established(&mut self, peer_id: &str) {
        self.current_established = self.current_established.saturating_sub(1);
        if let Some(count) = self.per_peer_counts.get_mut(peer_id) {
            *count = count.saturating_sub(1);
            if *count == 0 {
                self.per_peer_counts.remove(peer_id);
            }
        }
    }

    /// Try to acquire a pending (handshake) connection slot.
    pub fn try_acquire_pending(&mut self) -> bool {
        if self.current_pending >= self.max_pending {
            return false;
        }
        self.current_pending = self.current_pending.saturating_add(1);
        true
    }

    /// Release a pending connection slot (handshake completed or failed).
    pub fn release_pending(&mut self) {
        self.current_pending = self.current_pending.saturating_sub(1);
    }

    /// Promote a pending connection to established.
    ///
    /// Releases one pending slot and tries to acquire one established slot.
    /// Returns `true` if the established slot was granted.
    pub fn promote_to_established(&mut self, peer_id: &str) -> bool {
        self.release_pending();
        self.try_acquire_established(peer_id)
    }

    /// Current number of established connections.
    pub fn established_count(&self) -> u32 {
        self.current_established
    }

    /// Current number of pending connections.
    pub fn pending_count(&self) -> u32 {
        self.current_pending
    }

    /// Number of established connections for a specific peer.
    pub fn peer_count(&self, peer_id: &str) -> u32 {
        self.per_peer_counts.get(peer_id).copied().unwrap_or(0)
    }

    /// Remaining established connection slots.
    pub fn remaining_established(&self) -> u32 {
        self.max_established
            .saturating_sub(self.current_established)
    }

    /// Remaining pending connection slots.
    pub fn remaining_pending(&self) -> u32 {
        self.max_pending.saturating_sub(self.current_pending)
    }

    /// Whether the budget has any remaining capacity for new established
    /// connections.
    pub fn has_capacity(&self) -> bool {
        self.current_established < self.max_established
    }
}

// ── Tests ───────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;

    // ── Basic acquire/release ───────────────────────────────────────

    /// Acquiring within limits succeeds.
    #[test]
    fn acquire_within_limits() {
        let mut budget = ConnectionBudget::new(3, 5, 2);
        assert!(budget.try_acquire_established("a"));
        assert!(budget.try_acquire_established("a"));
        assert_eq!(budget.established_count(), 2);
        assert_eq!(budget.peer_count("a"), 2);
    }

    /// Per-peer limit is enforced.
    #[test]
    fn per_peer_limit_enforced() {
        let mut budget = ConnectionBudget::new(10, 5, 1);
        assert!(budget.try_acquire_established("a"));
        assert!(!budget.try_acquire_established("a")); // per-peer limit
                                                       // Different peer succeeds.
        assert!(budget.try_acquire_established("b"));
    }

    /// Total established limit is enforced.
    #[test]
    fn total_established_limit_enforced() {
        let mut budget = ConnectionBudget::new(2, 5, 2);
        assert!(budget.try_acquire_established("a"));
        assert!(budget.try_acquire_established("b"));
        assert!(!budget.try_acquire_established("c")); // total limit
    }

    /// Release frees up slots for reuse.
    #[test]
    fn release_frees_slot() {
        let mut budget = ConnectionBudget::new(1, 5, 1);
        assert!(budget.try_acquire_established("a"));
        assert!(!budget.try_acquire_established("b"));
        budget.release_established("a");
        assert!(budget.try_acquire_established("b"));
    }

    /// Release for unknown peer is a no-op (no underflow).
    #[test]
    fn release_unknown_peer_no_underflow() {
        let mut budget = ConnectionBudget::new(5, 5, 1);
        budget.release_established("unknown");
        assert_eq!(budget.established_count(), 0);
    }

    // ── Pending connections ─────────────────────────────────────────

    /// Pending connections have independent limits.
    #[test]
    fn pending_limit_independent() {
        let mut budget = ConnectionBudget::new(10, 2, 5);
        assert!(budget.try_acquire_pending());
        assert!(budget.try_acquire_pending());
        assert!(!budget.try_acquire_pending()); // pending limit
                                                // Established still works.
        assert!(budget.try_acquire_established("a"));
    }

    /// Promote moves a connection from pending to established.
    #[test]
    fn promote_decrements_pending_increments_established() {
        let mut budget = ConnectionBudget::new(5, 5, 2);
        assert!(budget.try_acquire_pending());
        assert_eq!(budget.pending_count(), 1);
        assert!(budget.promote_to_established("a"));
        assert_eq!(budget.pending_count(), 0);
        assert_eq!(budget.established_count(), 1);
    }

    // ── Error detail ────────────────────────────────────────────────

    /// Checked acquire returns structured error on failure.
    #[test]
    fn checked_acquire_returns_error_detail() {
        let mut budget = ConnectionBudget::new(1, 5, 1);
        assert!(budget.try_acquire_established_checked("a").is_ok());
        // Total limit hit.
        let err = budget.try_acquire_established_checked("b").unwrap_err();
        assert_eq!(err, BudgetExceeded::TotalEstablished { limit: 1 });
    }

    /// Per-peer error includes peer name.
    #[test]
    fn per_peer_error_includes_identity() {
        let mut budget = ConnectionBudget::new(10, 5, 1);
        assert!(budget.try_acquire_established_checked("alice").is_ok());
        let err = budget.try_acquire_established_checked("alice").unwrap_err();
        match err {
            BudgetExceeded::PerPeer { peer, limit } => {
                assert_eq!(peer, "alice");
                assert_eq!(limit, 1);
            }
            other => panic!("expected PerPeer, got {other:?}"),
        }
    }

    /// BudgetExceeded Display messages are human-readable.
    #[test]
    fn budget_exceeded_display() {
        let msg = BudgetExceeded::TotalEstablished { limit: 50 }.to_string();
        assert!(msg.contains("50"), "should contain limit: {msg}");
        let msg = BudgetExceeded::PerPeer {
            peer: "bob".into(),
            limit: 1,
        }
        .to_string();
        assert!(msg.contains("bob"), "should contain peer: {msg}");
    }

    // ── Capacity queries ────────────────────────────────────────────

    /// has_capacity and remaining_established are consistent.
    #[test]
    fn capacity_queries_consistent() {
        let mut budget = ConnectionBudget::new(3, 5, 3);
        assert!(budget.has_capacity());
        assert_eq!(budget.remaining_established(), 3);
        budget.try_acquire_established("a");
        budget.try_acquire_established("a");
        budget.try_acquire_established("a");
        assert!(!budget.has_capacity());
        assert_eq!(budget.remaining_established(), 0);
    }

    /// default_download produces sensible defaults.
    #[test]
    fn default_download_values() {
        let budget = ConnectionBudget::default_download();
        assert_eq!(budget.remaining_established(), 50);
        assert_eq!(budget.remaining_pending(), 10);
    }

    // ── Eclipse attack scenario ─────────────────────────────────────

    /// A single peer cannot monopolise all slots (eclipse attack
    /// prevention).
    ///
    /// libp2p's `with_max_established_per_peer` exists specifically for
    /// this. Without per-peer limits, an eclipse attacker fills all slots
    /// with their own connections, isolating the victim from honest peers.
    #[test]
    fn eclipse_attack_prevented() {
        let mut budget = ConnectionBudget::new(50, 10, 1);
        // Attacker tries to fill all 50 slots.
        assert!(budget.try_acquire_established("attacker"));
        // Per-peer limit: only 1 slot.
        for _ in 0..49 {
            assert!(!budget.try_acquire_established("attacker"));
        }
        // 49 slots remain for other peers.
        assert_eq!(budget.remaining_established(), 49);
    }
}
