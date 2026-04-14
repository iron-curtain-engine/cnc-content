// SPDX-License-Identifier: MIT OR Apache-2.0

//! Bilateral credit ledger — per-peer upload/download tracking for incentive-
//! compatible peer selection and choking.
//!
//! ## What
//!
//! Tracks `(uploaded_to_me, downloaded_from_me)` for each peer, producing a
//! credit modifier that rewards peers who contribute more than they consume.
//! The modifier is wired into the choking strategy's unchoke priority so
//! generous peers are preferentially unchoked.
//!
//! ## Why — eMule credit system lesson
//!
//! eMule tracks per-client transfer volumes across sessions. A "credit
//! modifier" biases the upload queue: clients that have uploaded more to us
//! than they've downloaded get priority. The formula prevents free-riding
//! without requiring a global currency or trust anchor.
//!
//! eMule's formula:
//! ```text
//! modifier = min(2 * uploaded_to_me / max(1 MB, downloaded_from_me), 10.0)
//! ```
//!
//! This is bounded [0, 10] and uses 1 MB as a floor to prevent division-by-
//! zero and to avoid extreme ratios from tiny transfers.
//!
//! ## How
//!
//! The [`CreditLedger`] maintains a `HashMap<PeerId, CreditEntry>` keyed by
//! cryptographic peer identity. The coordinator:
//!
//! 1. Calls [`record_received()`](CreditLedger::record_received) when downloading
//!    a piece from a peer (they upload to us).
//! 2. Calls [`record_sent()`](CreditLedger::record_sent) when uploading a piece
//!    to a peer (we upload to them).
//! 3. Queries [`credit_modifier()`](CreditLedger::credit_modifier) during choking
//!    evaluation to weight unchoke decisions.
//!
//! The ledger is serializable for cross-session persistence — a returning
//! peer retains its credit from previous sessions.

use std::collections::HashMap;

use crate::peer_id::PeerId;

// ── Constants ───────────────────────────────────────────────────────

/// Floor value for `downloaded_from_me` in the credit formula (1 MiB).
///
/// Prevents division by zero and avoids extreme modifier spikes from
/// tiny transfers. eMule uses 1 MB.
const DOWNLOAD_FLOOR_BYTES: u64 = 1_048_576;

/// Maximum credit modifier. Caps the advantage a generous peer can have
/// to prevent a single high-credit peer from monopolizing unchoke slots.
/// eMule uses 10.0.
const MAX_MODIFIER: f64 = 10.0;

// ── CreditEntry ─────────────────────────────────────────────────────

/// Per-peer credit tracking: bytes exchanged in each direction.
///
/// All fields use plain types for serialization compatibility (no Instant,
/// no serde dependency).
///
/// ```
/// use p2p_distribute::credit::CreditEntry;
///
/// let mut entry = CreditEntry::new();
/// entry.add_received(1_000_000); // peer uploaded 1 MB to us
/// entry.add_sent(500_000);       // we uploaded 500 KB to them
///
/// // Credit modifier: 2 * 1_000_000 / max(1_048_576, 500_000) ≈ 1.91
/// let modifier = entry.credit_modifier();
/// assert!(modifier > 1.0 && modifier < 10.0);
/// ```
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CreditEntry {
    /// Total bytes received from this peer (they uploaded to us).
    pub uploaded_to_me: u64,
    /// Total bytes sent to this peer (we uploaded to them).
    pub downloaded_from_me: u64,
    /// Last interaction timestamp as Unix epoch seconds (for expiry).
    pub last_seen_unix_secs: u64,
}

impl CreditEntry {
    /// Creates an empty credit entry (zero transfers).
    pub fn new() -> Self {
        Self {
            uploaded_to_me: 0,
            downloaded_from_me: 0,
            last_seen_unix_secs: 0,
        }
    }

    /// Records bytes received from this peer (they upload to us).
    pub fn add_received(&mut self, bytes: u64) {
        self.uploaded_to_me = self.uploaded_to_me.saturating_add(bytes);
    }

    /// Records bytes sent to this peer (we upload to them).
    pub fn add_sent(&mut self, bytes: u64) {
        self.downloaded_from_me = self.downloaded_from_me.saturating_add(bytes);
    }

    /// Updates the last-seen timestamp.
    pub fn touch(&mut self, unix_secs: u64) {
        self.last_seen_unix_secs = unix_secs;
    }

    /// Computes the eMule-style credit modifier.
    ///
    /// ```text
    /// modifier = min(2 * uploaded_to_me / max(FLOOR, downloaded_from_me), 10.0)
    /// ```
    ///
    /// Returns a value in [0.0, 10.0]:
    /// - 0.0 = peer has never uploaded to us
    /// - 1.0 = peer has uploaded roughly half of what we sent them
    /// - 10.0 = peer has been extremely generous (capped)
    pub fn credit_modifier(&self) -> f64 {
        let numerator = 2.0 * self.uploaded_to_me as f64;
        let denominator = (self.downloaded_from_me as f64).max(DOWNLOAD_FLOOR_BYTES as f64);
        (numerator / denominator).min(MAX_MODIFIER)
    }
}

impl Default for CreditEntry {
    fn default() -> Self {
        Self::new()
    }
}

// ── CreditLedger ────────────────────────────────────────────────────

/// Session-level (or cross-session) bilateral credit tracking.
///
/// Maps [`PeerId`] → [`CreditEntry`]. The coordinator uses this to bias
/// choking decisions toward generous peers.
///
/// ## Cross-session persistence
///
/// The ledger stores `PeerId` and `CreditEntry` using plain types. Consumers
/// can serialize/deserialize entries with any framework. Entries older than
/// a consumer-defined expiry window should be pruned to prevent stale credits
/// from granting trust to reassigned identities.
///
/// ```
/// use p2p_distribute::credit::CreditLedger;
/// use p2p_distribute::PeerId;
///
/// let mut ledger = CreditLedger::new();
/// let peer = PeerId::from_key_material(b"alice");
///
/// ledger.record_received(&peer, 10_000_000, 1000);
/// ledger.record_sent(&peer, 1_000_000, 1000);
///
/// // Alice uploaded 10 MB to us, we uploaded 1 MB back.
/// // modifier = min(2*10M / max(1_048_576, 1M), 10) → capped at 10.0
/// let modifier = ledger.credit_modifier(&peer);
/// assert!((modifier - 10.0).abs() < f64::EPSILON);
/// ```
#[derive(Debug, Clone, Default)]
pub struct CreditLedger {
    /// Per-peer credit entries keyed by cryptographic identity.
    entries: HashMap<PeerId, CreditEntry>,
}

impl CreditLedger {
    /// Creates an empty credit ledger.
    pub fn new() -> Self {
        Self::default()
    }

    /// Creates a ledger pre-loaded with prior session entries.
    pub fn from_entries(entries: HashMap<PeerId, CreditEntry>) -> Self {
        Self { entries }
    }

    /// Records bytes received from a peer (they upload to us).
    pub fn record_received(&mut self, peer_id: &PeerId, bytes: u64, now_unix_secs: u64) {
        let entry = self.entries.entry(*peer_id).or_default();
        entry.add_received(bytes);
        entry.touch(now_unix_secs);
    }

    /// Records bytes sent to a peer (we upload to them).
    pub fn record_sent(&mut self, peer_id: &PeerId, bytes: u64, now_unix_secs: u64) {
        let entry = self.entries.entry(*peer_id).or_default();
        entry.add_sent(bytes);
        entry.touch(now_unix_secs);
    }

    /// Returns the credit modifier for a peer.
    ///
    /// Returns 0.0 for unknown peers (no credit history).
    pub fn credit_modifier(&self, peer_id: &PeerId) -> f64 {
        self.entries
            .get(peer_id)
            .map(|e| e.credit_modifier())
            .unwrap_or(0.0)
    }

    /// Returns the credit entry for a peer, if it exists.
    pub fn get(&self, peer_id: &PeerId) -> Option<&CreditEntry> {
        self.entries.get(peer_id)
    }

    /// Number of peers tracked.
    pub fn peer_count(&self) -> usize {
        self.entries.len()
    }

    /// Prunes entries older than `max_age_secs` from `now_unix_secs`.
    ///
    /// Call periodically or at session start to prevent stale credits
    /// from accumulating indefinitely.
    pub fn prune(&mut self, now_unix_secs: u64, max_age_secs: u64) {
        let cutoff = now_unix_secs.saturating_sub(max_age_secs);
        self.entries
            .retain(|_, entry| entry.last_seen_unix_secs >= cutoff);
    }

    /// Returns all entries as a slice-compatible iterator.
    ///
    /// Useful for serialization at session end.
    pub fn iter(&self) -> impl Iterator<Item = (&PeerId, &CreditEntry)> {
        self.entries.iter()
    }

    /// Returns total bytes received across all peers.
    pub fn total_received(&self) -> u64 {
        self.entries
            .values()
            .map(|e| e.uploaded_to_me)
            .fold(0u64, u64::saturating_add)
    }

    /// Returns total bytes sent across all peers.
    pub fn total_sent(&self) -> u64 {
        self.entries
            .values()
            .map(|e| e.downloaded_from_me)
            .fold(0u64, u64::saturating_add)
    }
}

// ── Tests ───────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;

    fn test_peer(name: &[u8]) -> PeerId {
        PeerId::from_key_material(name)
    }

    // ── CreditEntry ─────────────────────────────────────────────────

    /// New entry has zero credit modifier.
    ///
    /// No bytes exchanged → modifier is 0.0.
    #[test]
    fn new_entry_zero_modifier() {
        let entry = CreditEntry::new();
        assert!((entry.credit_modifier() - 0.0).abs() < f64::EPSILON);
    }

    /// Received-only entry produces positive modifier.
    ///
    /// Peer uploaded to us but we haven't uploaded back. Floor applies.
    #[test]
    fn received_only_positive_modifier() {
        let mut entry = CreditEntry::new();
        entry.add_received(2_000_000);
        // modifier = 2 * 2M / max(1M, 0) = 4M / 1M = 4.0 (floor applies)
        let m = entry.credit_modifier();
        assert!((m - (2.0 * 2_000_000.0 / DOWNLOAD_FLOOR_BYTES as f64)).abs() < 0.01);
    }

    /// Equal exchange produces modifier of ~2.0.
    ///
    /// When uploaded_to_me == downloaded_from_me, modifier = 2 * X / max(FLOOR, X).
    #[test]
    fn equal_exchange_modifier() {
        let mut entry = CreditEntry::new();
        let amount = 5_000_000u64; // above floor
        entry.add_received(amount);
        entry.add_sent(amount);
        // modifier = 2 * 5M / max(1M, 5M) = 10M / 5M = 2.0
        assert!((entry.credit_modifier() - 2.0).abs() < 0.01);
    }

    /// Modifier is capped at MAX_MODIFIER (10.0).
    ///
    /// Even extremely generous peers can't get more than 10x priority.
    #[test]
    fn modifier_capped_at_max() {
        let mut entry = CreditEntry::new();
        entry.add_received(100_000_000); // 100 MB received
        entry.add_sent(1_000_000); // 1 MB sent
        assert!((entry.credit_modifier() - MAX_MODIFIER).abs() < f64::EPSILON);
    }

    /// Saturating addition prevents overflow.
    ///
    /// Very large byte counts should not wrap.
    #[test]
    fn saturating_add_no_overflow() {
        let mut entry = CreditEntry::new();
        entry.add_received(u64::MAX);
        entry.add_received(1);
        assert_eq!(entry.uploaded_to_me, u64::MAX);
    }

    // ── CreditLedger ────────────────────────────────────────────────

    /// Empty ledger returns 0 modifier for unknown peers.
    ///
    /// Unknown peers have no credit history.
    #[test]
    fn empty_ledger_zero_modifier() {
        let ledger = CreditLedger::new();
        let peer = test_peer(b"unknown");
        assert!((ledger.credit_modifier(&peer) - 0.0).abs() < f64::EPSILON);
    }

    /// Recording received creates an entry.
    ///
    /// First interaction with a peer auto-creates the entry.
    #[test]
    fn record_received_creates_entry() {
        let mut ledger = CreditLedger::new();
        let peer = test_peer(b"alice");
        ledger.record_received(&peer, 1_000_000, 100);
        assert_eq!(ledger.peer_count(), 1);
        assert!(ledger.credit_modifier(&peer) > 0.0);
    }

    /// Recording sent increases downloaded_from_me.
    #[test]
    fn record_sent_increases_downloaded() {
        let mut ledger = CreditLedger::new();
        let peer = test_peer(b"bob");
        ledger.record_sent(&peer, 500_000, 100);
        let entry = ledger.get(&peer).unwrap();
        assert_eq!(entry.downloaded_from_me, 500_000);
    }

    /// Bidirectional tracking computes correct modifier.
    #[test]
    fn bidirectional_modifier() {
        let mut ledger = CreditLedger::new();
        let peer = test_peer(b"charlie");
        ledger.record_received(&peer, 10_000_000, 100); // 10 MB in
        ledger.record_sent(&peer, 5_000_000, 100); // 5 MB out
                                                   // modifier = 2 * 10M / max(1M, 5M) = 20M / 5M = 4.0
        assert!((ledger.credit_modifier(&peer) - 4.0).abs() < 0.01);
    }

    /// Prune removes stale entries.
    ///
    /// Entries older than the max age are evicted to prevent stale credits
    /// from granting trust to reassigned identities.
    #[test]
    fn prune_removes_stale() {
        let mut ledger = CreditLedger::new();
        let old_peer = test_peer(b"old");
        let new_peer = test_peer(b"new");
        ledger.record_received(&old_peer, 1_000, 100); // seen at t=100
        ledger.record_received(&new_peer, 1_000, 500); // seen at t=500

        ledger.prune(600, 200); // cutoff = 400
        assert_eq!(ledger.peer_count(), 1);
        assert!(ledger.get(&old_peer).is_none());
        assert!(ledger.get(&new_peer).is_some());
    }

    /// Prune with max_age=0 removes everything except current-second entries.
    #[test]
    fn prune_zero_max_age() {
        let mut ledger = CreditLedger::new();
        let peer = test_peer(b"ephemeral");
        ledger.record_received(&peer, 1_000, 100);
        ledger.prune(200, 0); // cutoff = 200, entry at 100 < 200
        assert_eq!(ledger.peer_count(), 0);
    }

    /// Total received sums all peers' received bytes.
    #[test]
    fn total_received_aggregates() {
        let mut ledger = CreditLedger::new();
        ledger.record_received(&test_peer(b"a"), 1_000, 1);
        ledger.record_received(&test_peer(b"b"), 2_000, 1);
        assert_eq!(ledger.total_received(), 3_000);
    }

    /// Total sent sums all peers' sent bytes.
    #[test]
    fn total_sent_aggregates() {
        let mut ledger = CreditLedger::new();
        ledger.record_sent(&test_peer(b"a"), 500, 1);
        ledger.record_sent(&test_peer(b"b"), 1_500, 1);
        assert_eq!(ledger.total_sent(), 2_000);
    }

    // ── Determinism ─────────────────────────────────────────────────

    /// Credit modifier is deterministic for the same inputs.
    #[test]
    fn modifier_deterministic() {
        let mut entry = CreditEntry::new();
        entry.add_received(3_000_000);
        entry.add_sent(1_500_000);
        let m1 = entry.credit_modifier();
        let m2 = entry.credit_modifier();
        assert!((m1 - m2).abs() < f64::EPSILON);
    }
}
