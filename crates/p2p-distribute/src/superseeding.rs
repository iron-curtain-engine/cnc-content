// SPDX-License-Identifier: MIT OR Apache-2.0

//! BEP 16 super-seeding — optimised initial seeder mode.
//!
//! ## What
//!
//! Implements super-seed mode (BEP 16) where an initial seeder strategically
//! offers each piece to only one peer at a time, maximising piece diversity
//! in the swarm as quickly as possible. Once a piece has been seen by at
//! least two peers, it can propagate virally through normal rarest-first
//! selection.
//!
//! ## Why — initial seeding for IC content bootstrap
//!
//! Iron Curtain's content distribution starts with HTTP mirrors as the sole
//! source. When BitTorrent is used, the bridge node (or content-bootstrap
//! mirror) is often the only seed at first. Super-seeding is critical here:
//!
//! - **Maximise piece diversity.** Without super-seeding, naive BT clients
//!   might all request piece 0, then piece 1, etc. from the single seed.
//!   With N leechers and 1 seed, normal seeding wastes N-1 upload slots per
//!   piece on redundant transfers. Super-seeding ensures each upload slot
//!   sends a *different* piece, so after one round the swarm has N distinct
//!   pieces and can peer-exchange them.
//! - **Reduce mirror load.** The content-bootstrap HTTP mirrors have limited
//!   bandwidth (community-funded). Super-seeding means each piece is uploaded
//!   from the mirror at most once or twice before swarm members take over.
//! - **Faster swarm bootstrap.** A swarm of 20 leechers + 1 super-seed
//!   reaches full replication in roughly `ceil(pieces/20)` rounds instead of
//!   `pieces` rounds with naive seeding.
//!
//! ## How
//!
//! - [`SuperSeedState`]: Tracks which pieces have been offered to which
//!   peers, and which pieces have been confirmed as propagated.
//! - [`PieceOffer`]: A record of a piece offered to a specific peer.
//! - The super-seed selects which piece to advertise to each peer using
//!   the strategy: prefer pieces not yet offered to anyone, then pieces
//!   offered but not yet confirmed propagated.

use std::time::{Duration, Instant};

// ── Constants ───────────────────────────────────────────────────────

/// Maximum number of peers tracked in super-seed state.
const MAX_SUPER_SEED_PEERS: usize = 200;

/// Timeout for considering an offered piece as "stuck" (peer didn't
/// request it or propagate it within this time).
const OFFER_TIMEOUT: Duration = Duration::from_secs(120);

/// Minimum number of peers that must have a piece before it's considered
/// propagated. BEP 16 suggests 2 (the original seeder + at least one
/// downstream peer who has re-shared it).
const PROPAGATION_THRESHOLD: u32 = 2;

// ── Piece offer ─────────────────────────────────────────────────────

/// Record of a piece offered to a specific peer via have-message.
///
/// In super-seed mode, the seeder lies about its bitfield — it claims
/// to have no pieces, then selectively sends `have` messages to
/// individual peers. This record tracks these selective advertisements.
#[derive(Debug, Clone)]
pub struct PieceOffer {
    /// Piece index that was offered.
    piece: u32,
    /// Peer ID the piece was offered to.
    peer_id: [u8; 20],
    /// When the offer was made.
    offered_at: Instant,
    /// Whether the peer has requested this piece.
    requested: bool,
}

impl PieceOffer {
    /// Creates a new piece offer record.
    pub fn new(piece: u32, peer_id: [u8; 20], now: Instant) -> Self {
        Self {
            piece,
            peer_id,
            offered_at: now,
            requested: false,
        }
    }

    /// Returns the piece index.
    pub fn piece(&self) -> u32 {
        self.piece
    }

    /// Returns the peer ID this was offered to.
    pub fn peer_id(&self) -> &[u8; 20] {
        &self.peer_id
    }

    /// Returns whether the offer has timed out.
    pub fn is_expired(&self, now: Instant) -> bool {
        now.duration_since(self.offered_at) > OFFER_TIMEOUT
    }

    /// Marks this offer as requested by the peer.
    pub fn mark_requested(&mut self) {
        self.requested = true;
    }

    /// Returns whether the peer has requested this piece.
    pub fn was_requested(&self) -> bool {
        self.requested
    }
}

// ── Super-seed state ────────────────────────────────────────────────

/// Tracks super-seed mode state for one torrent.
///
/// The super-seed maintains a virtual bitfield per peer. Initially, the
/// seeder advertises zero pieces to all peers. It then selectively sends
/// `have` messages to individual peers, choosing pieces that maximise
/// diversity across the swarm.
///
/// ```
/// use p2p_distribute::superseeding::SuperSeedState;
/// use std::time::Instant;
///
/// let now = Instant::now();
/// let mut state = SuperSeedState::new(100, now);
///
/// // Register a peer.
/// let peer = [1u8; 20];
/// state.register_peer(peer);
///
/// // Get the piece we should offer this peer.
/// let piece = state.select_piece_for(&peer, now);
/// assert!(piece.is_some(), "should offer a piece to the new peer");
/// ```
pub struct SuperSeedState {
    /// Total number of pieces in the torrent.
    piece_count: u32,
    /// Active piece offers (piece → peer assignments).
    offers: Vec<PieceOffer>,
    /// How many distinct peers have each piece (observed via have messages
    /// from leechers). Index = piece index, value = peer count.
    propagation: Vec<u32>,
    /// Set of registered peer IDs.
    peers: Vec<[u8; 20]>,
    /// Which pieces have been offered at least once.
    offered_pieces: Vec<bool>,
}

impl SuperSeedState {
    /// Creates a new super-seed state for a torrent.
    pub fn new(piece_count: u32, _now: Instant) -> Self {
        Self {
            piece_count,
            offers: Vec::new(),
            propagation: vec![0; piece_count as usize],
            peers: Vec::new(),
            offered_pieces: vec![false; piece_count as usize],
        }
    }

    /// Registers a peer for super-seed tracking.
    pub fn register_peer(&mut self, peer_id: [u8; 20]) {
        if self.peers.contains(&peer_id) {
            return;
        }
        if self.peers.len() >= MAX_SUPER_SEED_PEERS {
            return; // Cap reached.
        }
        self.peers.push(peer_id);
    }

    /// Removes a peer from tracking.
    pub fn remove_peer(&mut self, peer_id: &[u8; 20]) {
        self.peers.retain(|p| p != peer_id);
        self.offers.retain(|o| &o.peer_id != peer_id);
    }

    /// Selects the best piece to offer to a specific peer.
    ///
    /// Strategy (in priority order):
    /// 1. A piece never offered to any peer (maximises diversity).
    /// 2. A piece not yet offered to *this* peer, preferring pieces with
    ///    the lowest propagation count.
    /// 3. `None` if all pieces have been offered to this peer.
    pub fn select_piece_for(&mut self, peer_id: &[u8; 20], now: Instant) -> Option<u32> {
        // Already offered pieces to this peer (active, non-expired).
        let already_offered: std::collections::HashSet<u32> = self
            .offers
            .iter()
            .filter(|o| &o.peer_id == peer_id && !o.is_expired(now))
            .map(|o| o.piece)
            .collect();

        // Strategy 1: never-offered piece.
        let never_offered = self
            .offered_pieces
            .iter()
            .enumerate()
            .find(|(_, offered)| !**offered)
            .map(|(i, _)| i as u32);

        if let Some(piece) = never_offered {
            if !already_offered.contains(&piece) {
                self.record_offer(piece, *peer_id, now);
                return Some(piece);
            }
        }

        // Strategy 2: least-propagated piece not yet offered to this peer.
        let mut candidates: Vec<(u32, u32)> = (0..self.piece_count)
            .filter(|&i| !already_offered.contains(&i))
            .filter_map(|i| self.propagation.get(i as usize).map(|&count| (i, count)))
            .collect();

        // Sort by propagation count ascending (rarest first).
        candidates.sort_by_key(|&(_, count)| count);

        if let Some(&(piece, _)) = candidates.first() {
            self.record_offer(piece, *peer_id, now);
            return Some(piece);
        }

        None
    }

    /// Records that a piece was offered to a peer.
    fn record_offer(&mut self, piece: u32, peer_id: [u8; 20], now: Instant) {
        self.offers.push(PieceOffer::new(piece, peer_id, now));
        if let Some(flag) = self.offered_pieces.get_mut(piece as usize) {
            *flag = true;
        }
    }

    /// Records that a peer has requested a previously-offered piece.
    ///
    /// This confirms the peer saw our `have` message and is downloading
    /// the piece.
    pub fn mark_requested(&mut self, piece: u32, peer_id: &[u8; 20]) {
        for offer in &mut self.offers {
            if offer.piece == piece && &offer.peer_id == peer_id {
                offer.mark_requested();
                return;
            }
        }
    }

    /// Records that a peer now has a piece (observed via their `have`
    /// message to us).
    ///
    /// This is how we learn that a piece has propagated beyond the
    /// initial recipient.
    pub fn record_peer_has_piece(&mut self, piece: u32) {
        if let Some(count) = self.propagation.get_mut(piece as usize) {
            *count = count.saturating_add(1);
        }
    }

    /// Returns the number of pieces that have been sufficiently propagated.
    pub fn propagated_count(&self) -> u32 {
        self.propagation
            .iter()
            .filter(|&&count| count >= PROPAGATION_THRESHOLD)
            .count() as u32
    }

    /// Returns whether super-seeding is complete (all pieces propagated).
    pub fn is_complete(&self) -> bool {
        self.propagated_count() == self.piece_count
    }

    /// Returns overall progress as a fraction in [0.0, 1.0].
    pub fn progress(&self) -> f64 {
        if self.piece_count == 0 {
            return 1.0;
        }
        self.propagated_count() as f64 / self.piece_count as f64
    }

    /// Cleans up expired offers.
    pub fn cleanup_expired(&mut self, now: Instant) -> usize {
        let before = self.offers.len();
        self.offers.retain(|o| !o.is_expired(now));
        before.saturating_sub(self.offers.len())
    }

    /// Returns the number of active (non-expired) offers.
    pub fn active_offer_count(&self, now: Instant) -> usize {
        self.offers.iter().filter(|o| !o.is_expired(now)).count()
    }

    /// Returns the number of registered peers.
    pub fn peer_count(&self) -> usize {
        self.peers.len()
    }

    /// Returns the total piece count.
    pub fn piece_count(&self) -> u32 {
        self.piece_count
    }

    /// Returns the propagation count for a piece.
    pub fn propagation_count(&self, piece: u32) -> Option<u32> {
        self.propagation.get(piece as usize).copied()
    }
}

// ── Tests ───────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;

    // ── PieceOffer ──────────────────────────────────────────────────

    /// Offer expiry detection.
    ///
    /// Offers that exceed the timeout should be considered expired.
    #[test]
    fn offer_expires() {
        let now = Instant::now();
        let offer = PieceOffer::new(0, [1u8; 20], now);

        assert!(!offer.is_expired(now));
        let later = now + OFFER_TIMEOUT + Duration::from_secs(1);
        assert!(offer.is_expired(later));
    }

    /// Offer request tracking.
    ///
    /// Marking an offer as requested should be reflected in the state.
    #[test]
    fn offer_request_tracking() {
        let now = Instant::now();
        let mut offer = PieceOffer::new(0, [1u8; 20], now);

        assert!(!offer.was_requested());
        offer.mark_requested();
        assert!(offer.was_requested());
    }

    // ── SuperSeedState ──────────────────────────────────────────────

    /// New state offers pieces starting from index 0.
    ///
    /// The first peer should receive the first never-offered piece.
    #[test]
    fn first_offer_is_piece_zero() {
        let now = Instant::now();
        let mut state = SuperSeedState::new(10, now);
        state.register_peer([1u8; 20]);

        let piece = state.select_piece_for(&[1u8; 20], now);
        assert_eq!(piece, Some(0));
    }

    /// Different peers get different pieces.
    ///
    /// This is the core super-seeding invariant: maximise piece diversity
    /// by offering each peer a different piece.
    #[test]
    fn different_peers_get_different_pieces() {
        let now = Instant::now();
        let mut state = SuperSeedState::new(10, now);

        state.register_peer([1u8; 20]);
        state.register_peer([2u8; 20]);

        let p1 = state.select_piece_for(&[1u8; 20], now);
        let p2 = state.select_piece_for(&[2u8; 20], now);

        assert_ne!(p1, p2, "different peers should get different pieces");
    }

    /// All pieces eventually get offered.
    ///
    /// With enough peers, every piece should be offered at least once.
    #[test]
    fn all_pieces_offered() {
        let now = Instant::now();
        let mut state = SuperSeedState::new(5, now);

        for i in 0..5u8 {
            state.register_peer([i + 1; 20]);
            state.select_piece_for(&[i + 1; 20], now);
        }

        let offered_count = state.offered_pieces.iter().filter(|&&f| f).count();
        assert_eq!(offered_count, 5, "all 5 pieces should be offered");
    }

    /// Propagation tracking and completion.
    ///
    /// When enough peers have each piece, super-seeding should be complete.
    #[test]
    fn propagation_completes() {
        let now = Instant::now();
        let mut state = SuperSeedState::new(3, now);

        assert!(!state.is_complete());
        assert!((state.progress() - 0.0).abs() < f64::EPSILON);

        // Simulate propagation for all pieces.
        for piece in 0..3 {
            for _ in 0..PROPAGATION_THRESHOLD {
                state.record_peer_has_piece(piece);
            }
        }

        assert!(state.is_complete());
        assert!((state.progress() - 1.0).abs() < f64::EPSILON);
    }

    /// Cleanup removes expired offers.
    ///
    /// Expired offers must be cleaned up to avoid stale state.
    #[test]
    fn cleanup_expired_offers() {
        let now = Instant::now();
        let mut state = SuperSeedState::new(10, now);
        state.register_peer([1u8; 20]);
        state.select_piece_for(&[1u8; 20], now);

        assert_eq!(state.active_offer_count(now), 1);

        let later = now + OFFER_TIMEOUT + Duration::from_secs(1);
        let cleaned = state.cleanup_expired(later);
        assert_eq!(cleaned, 1);
    }

    /// Peer removal cleans up offers.
    ///
    /// When a peer disconnects, its offers should be removed.
    #[test]
    fn remove_peer_cleans_offers() {
        let now = Instant::now();
        let mut state = SuperSeedState::new(10, now);
        state.register_peer([1u8; 20]);
        state.select_piece_for(&[1u8; 20], now);

        state.remove_peer(&[1u8; 20]);
        assert_eq!(state.peer_count(), 0);
        assert_eq!(state.active_offer_count(now), 0);
    }

    /// Duplicate peer registration is idempotent.
    ///
    /// Re-registering the same peer should not create duplicates.
    #[test]
    fn duplicate_peer_noop() {
        let now = Instant::now();
        let mut state = SuperSeedState::new(10, now);
        state.register_peer([1u8; 20]);
        state.register_peer([1u8; 20]);
        assert_eq!(state.peer_count(), 1);
    }

    /// Peer cap prevents unbounded growth.
    ///
    /// With many peers connecting, the peer list must be bounded.
    #[test]
    fn peer_cap_enforced() {
        let now = Instant::now();
        let mut state = SuperSeedState::new(10, now);

        for i in 0..=MAX_SUPER_SEED_PEERS {
            let mut id = [0u8; 20];
            id[0] = (i % 256) as u8;
            id[1] = (i / 256) as u8;
            state.register_peer(id);
        }

        assert!(state.peer_count() <= MAX_SUPER_SEED_PEERS);
    }

    /// Mark requested updates offer state.
    ///
    /// When a peer requests an offered piece, we track it for diagnostics.
    #[test]
    fn mark_requested_tracked() {
        let now = Instant::now();
        let mut state = SuperSeedState::new(10, now);
        state.register_peer([1u8; 20]);
        let piece = state.select_piece_for(&[1u8; 20], now).unwrap();

        state.mark_requested(piece, &[1u8; 20]);

        let offer = state.offers.iter().find(|o| o.piece == piece).unwrap();
        assert!(offer.was_requested());
    }

    /// Least-propagated piece preferred on second round.
    ///
    /// After all pieces are offered once, re-offers should prefer pieces
    /// with the lowest propagation count.
    #[test]
    fn least_propagated_preferred() {
        let now = Instant::now();
        let mut state = SuperSeedState::new(3, now);
        state.register_peer([1u8; 20]);

        // Offer all 3 pieces once.
        state.select_piece_for(&[1u8; 20], now);
        state.select_piece_for(&[1u8; 20], now);
        state.select_piece_for(&[1u8; 20], now);

        // Propagate pieces 0 and 1 but not 2.
        state.record_peer_has_piece(0);
        state.record_peer_has_piece(0);
        state.record_peer_has_piece(1);
        state.record_peer_has_piece(1);

        // Expire all offers so the peer can be offered again.
        let later = now + OFFER_TIMEOUT + Duration::from_secs(1);
        state.cleanup_expired(later);

        // New peer should get piece 2 (least propagated).
        state.register_peer([2u8; 20]);
        let piece = state.select_piece_for(&[2u8; 20], later);
        assert_eq!(piece, Some(2), "should prefer least-propagated piece");
    }

    /// Zero pieces is a valid edge case.
    ///
    /// Empty torrents should be immediately complete.
    #[test]
    fn zero_pieces_complete() {
        let now = Instant::now();
        let state = SuperSeedState::new(0, now);
        assert!(state.is_complete());
        assert!((state.progress() - 1.0).abs() < f64::EPSILON);
    }

    /// Propagation count query.
    ///
    /// Per-piece propagation must be queryable for diagnostics.
    #[test]
    fn propagation_count_query() {
        let now = Instant::now();
        let mut state = SuperSeedState::new(5, now);

        assert_eq!(state.propagation_count(0), Some(0));
        state.record_peer_has_piece(0);
        assert_eq!(state.propagation_count(0), Some(1));
        assert_eq!(state.propagation_count(99), None);
    }
}
