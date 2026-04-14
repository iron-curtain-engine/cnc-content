// SPDX-License-Identifier: MIT OR Apache-2.0

//! Endgame mode — aggressive completion strategy for the download tail.
//!
//! ## What
//!
//! Implements the BitTorrent endgame strategy (BEP 3 § endgame mode).
//! When a download enters the final phase (few pieces remaining), the
//! coordinator switches from requesting each block from one peer to
//! requesting remaining blocks from **all** eligible peers simultaneously.
//! When a block arrives, duplicate requests are cancelled.
//!
//! ## Why — the "last piece" problem
//!
//! In normal mode, each block is requested from exactly one peer. If that
//! peer is slow or disconnects, the block must be re-requested after a
//! timeout. Near the end of a download, this can cause a long tail: the
//! last few pieces are stuck behind slow peers while fast peers sit idle.
//!
//! Endgame mode solves this by broadcasting requests for remaining blocks
//! to all peers. The first response wins, and cancel messages are sent
//! for the duplicates. This wastes a small amount of bandwidth (duplicate
//! requests for the same block) but eliminates the tail latency that
//! dominates user-perceived download time.
//!
//! ## How
//!
//! - [`EndgameMode`]: State tracker that decides when to enter endgame
//!   mode and manages duplicate request tracking.
//! - [`EndgameAction`]: Actions the coordinator should take (broadcast
//!   requests, cancel duplicates).
//! - The threshold for entering endgame is configurable but defaults
//!   to when the number of remaining blocks drops below the number of
//!   connected peers — at that point, there are more workers than work
//!   items.

use std::collections::{HashMap, HashSet};

// ── Constants ───────────────────────────────────────────────────────

/// Default endgame threshold: enter endgame when remaining blocks
/// is less than or equal to this multiple of connected peers.
const DEFAULT_ENDGAME_PEER_MULTIPLIER: u32 = 2;

/// Minimum number of remaining pieces before endgame can activate.
/// Prevents premature activation on very small torrents.
const MIN_REMAINING_FOR_ENDGAME: u32 = 1;

// ── Block identifier ────────────────────────────────────────────────

/// Identifies a specific block within a torrent.
///
/// A block is a sub-piece chunk, typically 16 KiB. This is the unit
/// of request/response in the BT wire protocol.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct BlockId {
    /// Piece index (zero-based).
    pub piece: u32,
    /// Byte offset within the piece.
    pub offset: u32,
}

impl BlockId {
    /// Creates a new block identifier.
    pub fn new(piece: u32, offset: u32) -> Self {
        Self { piece, offset }
    }
}

// ── Endgame action ──────────────────────────────────────────────────

/// Action the coordinator should perform in endgame mode.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum EndgameAction {
    /// Send a request for this block to additional peers.
    BroadcastRequest {
        /// The block to request.
        block: BlockId,
    },
    /// Cancel a previously-sent request for this block at the given peer.
    CancelDuplicate {
        /// The block that was received.
        block: BlockId,
        /// Peer ID to send the cancel to.
        peer_id: [u8; 20],
    },
}

// ── Endgame mode ────────────────────────────────────────────────────

/// Endgame mode state tracker.
///
/// Tracks which blocks have been requested from which peers, and
/// produces actions when blocks arrive (cancel duplicates) or when
/// new peers become available (broadcast remaining requests).
///
/// ```
/// use p2p_distribute::endgame::{EndgameMode, BlockId};
///
/// let mut eg = EndgameMode::new(10); // 10 total pieces
///
/// // Not yet in endgame — too many pieces remaining.
/// assert!(!eg.is_active());
///
/// // Simulate progress: only 2 pieces left, 5 peers connected.
/// eg.update_remaining(2, 5);
/// assert!(eg.is_active());
/// ```
pub struct EndgameMode {
    /// Total piece count for the torrent.
    total_pieces: u32,
    /// Whether endgame mode is currently active.
    active: bool,
    /// For each block, which peers have been sent a request.
    /// Only populated while endgame is active.
    pending: HashMap<BlockId, HashSet<[u8; 20]>>,
    /// Peer multiplier threshold for activation.
    peer_multiplier: u32,
}

impl EndgameMode {
    /// Creates a new endgame tracker for a torrent.
    pub fn new(total_pieces: u32) -> Self {
        Self {
            total_pieces,
            active: false,
            pending: HashMap::new(),
            peer_multiplier: DEFAULT_ENDGAME_PEER_MULTIPLIER,
        }
    }

    /// Creates an endgame tracker with a custom peer multiplier.
    ///
    /// Endgame activates when `remaining_pieces <= peer_count * multiplier`.
    pub fn with_multiplier(total_pieces: u32, multiplier: u32) -> Self {
        Self {
            total_pieces,
            active: false,
            pending: HashMap::new(),
            peer_multiplier: multiplier,
        }
    }

    /// Returns whether endgame mode is currently active.
    pub fn is_active(&self) -> bool {
        self.active
    }

    /// Returns the total piece count.
    pub fn total_pieces(&self) -> u32 {
        self.total_pieces
    }

    /// Updates the remaining piece count and peer count, potentially
    /// activating or deactivating endgame mode.
    ///
    /// Returns `true` if endgame mode just activated (was off, now on).
    pub fn update_remaining(&mut self, remaining_pieces: u32, peer_count: u32) -> bool {
        let threshold = peer_count.saturating_mul(self.peer_multiplier);
        let was_active = self.active;

        // Activate when few pieces remain relative to peer count.
        self.active =
            remaining_pieces >= MIN_REMAINING_FOR_ENDGAME && remaining_pieces <= threshold;

        // If we deactivate (e.g. peers dropped), clear pending state.
        if !self.active {
            self.pending.clear();
        }

        !was_active && self.active
    }

    /// Records that a block request was sent to a peer.
    ///
    /// Call this for every request sent while in endgame mode.
    pub fn record_request(&mut self, block: BlockId, peer_id: [u8; 20]) {
        if !self.active {
            return;
        }
        self.pending.entry(block).or_default().insert(peer_id);
    }

    /// Records that a block was received, returning cancel actions for
    /// duplicate requests at other peers.
    ///
    /// The coordinator should send Cancel messages for each returned action.
    pub fn block_received(&mut self, block: BlockId, from_peer: &[u8; 20]) -> Vec<EndgameAction> {
        if !self.active {
            return Vec::new();
        }

        let mut actions = Vec::new();

        if let Some(peers) = self.pending.remove(&block) {
            // Cancel at every peer except the one that delivered.
            for peer_id in peers {
                if &peer_id != from_peer {
                    actions.push(EndgameAction::CancelDuplicate { block, peer_id });
                }
            }
        }

        actions
    }

    /// Returns blocks that should be broadcast-requested to a new peer.
    ///
    /// When a new peer becomes unchoked during endgame, send it requests
    /// for all pending blocks it hasn't been asked for yet.
    pub fn blocks_for_new_peer(&mut self, peer_id: [u8; 20]) -> Vec<EndgameAction> {
        if !self.active {
            return Vec::new();
        }

        let mut actions = Vec::new();

        for (block, peers) in &mut self.pending {
            if !peers.contains(&peer_id) {
                peers.insert(peer_id);
                actions.push(EndgameAction::BroadcastRequest { block: *block });
            }
        }

        actions
    }

    /// Returns the number of pending blocks being tracked.
    pub fn pending_block_count(&self) -> usize {
        self.pending.len()
    }

    /// Removes a peer from all pending request tracking.
    ///
    /// Call when a peer disconnects. The blocks remain pending but
    /// the peer is no longer a candidate for cancel messages.
    pub fn remove_peer(&mut self, peer_id: &[u8; 20]) {
        for peers in self.pending.values_mut() {
            peers.remove(peer_id);
        }
    }

    /// Resets endgame mode (e.g. on download pause).
    pub fn reset(&mut self) {
        self.active = false;
        self.pending.clear();
    }
}

// ── Tests ───────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;

    // ── Activation ──────────────────────────────────────────────────

    /// Endgame activates when remaining pieces drop below threshold.
    ///
    /// With 5 peers and multiplier 2, threshold is 10 pieces.
    #[test]
    fn activates_at_threshold() {
        let mut eg = EndgameMode::new(100);
        assert!(!eg.is_active());

        // 10 remaining, 5 peers → threshold = 10 → activates.
        let activated = eg.update_remaining(10, 5);
        assert!(activated);
        assert!(eg.is_active());
    }

    /// Endgame does not activate when many pieces remain.
    #[test]
    fn does_not_activate_early() {
        let mut eg = EndgameMode::new(100);
        eg.update_remaining(50, 5);
        assert!(!eg.is_active());
    }

    /// Endgame does not activate with zero remaining pieces.
    ///
    /// Zero remaining means the download is complete — no endgame needed.
    #[test]
    fn zero_remaining_not_active() {
        let mut eg = EndgameMode::new(100);
        eg.update_remaining(0, 10);
        assert!(!eg.is_active());
    }

    /// Custom multiplier adjusts the threshold.
    #[test]
    fn custom_multiplier() {
        let mut eg = EndgameMode::with_multiplier(100, 1);
        // 5 remaining, 5 peers, multiplier 1 → threshold = 5 → activates.
        eg.update_remaining(5, 5);
        assert!(eg.is_active());

        // 6 remaining → above threshold → deactivates.
        eg.update_remaining(6, 5);
        assert!(!eg.is_active());
    }

    // ── Duplicate cancellation ──────────────────────────────────────

    /// Receiving a block produces cancel actions for other peers.
    ///
    /// This is the core endgame mechanism: cancel duplicates to save
    /// bandwidth.
    #[test]
    fn cancel_duplicates_on_receive() {
        let mut eg = EndgameMode::new(10);
        eg.update_remaining(2, 5);
        assert!(eg.is_active());

        let block = BlockId::new(8, 0);
        let peer_a = [1u8; 20];
        let peer_b = [2u8; 20];
        let peer_c = [3u8; 20];

        eg.record_request(block, peer_a);
        eg.record_request(block, peer_b);
        eg.record_request(block, peer_c);

        // Peer A delivers — expect cancels for B and C.
        let actions = eg.block_received(block, &peer_a);
        assert_eq!(actions.len(), 2);

        let cancel_peers: HashSet<[u8; 20]> = actions
            .iter()
            .filter_map(|a| match a {
                EndgameAction::CancelDuplicate { peer_id, .. } => Some(*peer_id),
                _ => None,
            })
            .collect();
        assert!(cancel_peers.contains(&peer_b));
        assert!(cancel_peers.contains(&peer_c));
    }

    /// No cancels when only one peer had the request.
    #[test]
    fn no_cancels_single_peer() {
        let mut eg = EndgameMode::new(10);
        eg.update_remaining(2, 5);

        let block = BlockId::new(8, 0);
        let peer = [1u8; 20];
        eg.record_request(block, peer);

        let actions = eg.block_received(block, &peer);
        assert!(actions.is_empty());
    }

    // ── Broadcast for new peer ──────────────────────────────────────

    /// New peer receives broadcast requests for all pending blocks.
    #[test]
    fn broadcast_to_new_peer() {
        let mut eg = EndgameMode::new(10);
        eg.update_remaining(2, 5);

        let block_a = BlockId::new(8, 0);
        let block_b = BlockId::new(9, 0);
        let existing_peer = [1u8; 20];

        eg.record_request(block_a, existing_peer);
        eg.record_request(block_b, existing_peer);

        let new_peer = [2u8; 20];
        let actions = eg.blocks_for_new_peer(new_peer);
        assert_eq!(actions.len(), 2);
    }

    /// New peer does not get blocks already requested from them.
    #[test]
    fn no_duplicate_broadcast() {
        let mut eg = EndgameMode::new(10);
        eg.update_remaining(2, 5);

        let block = BlockId::new(8, 0);
        let peer = [1u8; 20];
        eg.record_request(block, peer);

        // Same peer asks again — no new broadcasts.
        let actions = eg.blocks_for_new_peer(peer);
        assert!(actions.is_empty());
    }

    // ── Peer removal ────────────────────────────────────────────────

    /// Removed peer is excluded from cancel actions.
    #[test]
    fn remove_peer_excludes_cancels() {
        let mut eg = EndgameMode::new(10);
        eg.update_remaining(2, 5);

        let block = BlockId::new(8, 0);
        let peer_a = [1u8; 20];
        let peer_b = [2u8; 20];

        eg.record_request(block, peer_a);
        eg.record_request(block, peer_b);

        eg.remove_peer(&peer_b);

        // Peer A delivers — peer B was removed, no cancel for B.
        let actions = eg.block_received(block, &peer_a);
        assert!(actions.is_empty());
    }

    // ── Reset ───────────────────────────────────────────────────────

    /// Reset clears all state.
    #[test]
    fn reset_clears_state() {
        let mut eg = EndgameMode::new(10);
        eg.update_remaining(2, 5);
        assert!(eg.is_active());

        eg.record_request(BlockId::new(8, 0), [1u8; 20]);
        eg.reset();

        assert!(!eg.is_active());
        assert_eq!(eg.pending_block_count(), 0);
    }

    // ── Inactive mode ───────────────────────────────────────────────

    /// Operations are no-ops when endgame is not active.
    #[test]
    fn inactive_operations_noop() {
        let mut eg = EndgameMode::new(100);
        assert!(!eg.is_active());

        let block = BlockId::new(0, 0);
        eg.record_request(block, [1u8; 20]);
        assert_eq!(eg.pending_block_count(), 0);

        let actions = eg.block_received(block, &[1u8; 20]);
        assert!(actions.is_empty());

        let broadcasts = eg.blocks_for_new_peer([2u8; 20]);
        assert!(broadcasts.is_empty());
    }

    /// Pending block count tracks correctly.
    #[test]
    fn pending_count() {
        let mut eg = EndgameMode::new(10);
        eg.update_remaining(3, 5);

        eg.record_request(BlockId::new(7, 0), [1u8; 20]);
        eg.record_request(BlockId::new(8, 0), [1u8; 20]);
        eg.record_request(BlockId::new(9, 0), [1u8; 20]);

        assert_eq!(eg.pending_block_count(), 3);

        eg.block_received(BlockId::new(8, 0), &[1u8; 20]);
        assert_eq!(eg.pending_block_count(), 2);
    }
}
