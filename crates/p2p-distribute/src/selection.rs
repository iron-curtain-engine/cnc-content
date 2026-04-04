// SPDX-License-Identifier: MIT OR Apache-2.0

//! Piece selection strategies — rarest-first with streaming priority
//! (BitTorrent + DASH/HLS hybrid).
//!
//! ## What
//!
//! Pure functions that rank pieces by a combined score of rarity (BT
//! rarest-first) and streaming priority (DASH playhead urgency). The
//! coordinator calls `select_next_piece()` instead of a sequential scan
//! to decide which piece to request next.
//!
//! ## Why — BT rarest-first is the #1 swarm health algorithm
//!
//! In a multi-peer swarm, sequential downloading causes "piece starvation":
//! all peers request the same early pieces, leaving later pieces with zero
//! replicas. When the original seeder leaves, those pieces become
//! permanently unavailable. Rarest-first ensures every piece has maximal
//! redundancy, which is what keeps a swarm alive after the initial seeder
//! departs.
//!
//! ## How — selection algorithm
//!
//! 1. Compute rarity scores from peer bitfields (`rarity_scores()`).
//! 2. For each needed piece, compute: `score = priority_weight × (max_rarity + 1) + rarity_inverse`.
//! 3. Sort by score (descending); break ties by piece index (ascending,
//!    for streaming bias toward earlier pieces).
//! 4. Return the highest-scoring piece that at least one eligible peer has.
//!
//! ## What about random tie-breaking?
//!
//! BT clients randomize among equally-rare pieces to prevent all peers from
//! requesting the same rarest piece simultaneously. We skip this because:
//! - Our swarms are small (content delivery, not file sharing).
//! - Deterministic selection is easier to test and reason about.
//! - The streaming priority multiplier already breaks most ties.

use crate::bitfield::{rarity_scores, PeerBitfield};
use crate::priority::PiecePriorityMap;

/// Result of piece selection: which piece to request and its score.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct PieceSelection {
    /// Piece index to request.
    pub piece_index: u32,
    /// Combined score (priority-dominant, additive formula). Higher is better.
    pub score: u64,
}

/// Selects the best piece to download next.
///
/// ## Parameters
///
/// - `needed`: slice of piece indices that still need downloading (not
///   Done, not InFlight).
/// - `bitfields`: per-peer bitfields (one per connected peer).
/// - `priority_map`: streaming-aware priority assignments.
/// - `piece_count`: total number of pieces in the torrent.
///
/// ## Returns
///
/// The highest-scoring piece from `needed` that at least one peer in
/// `bitfields` has. Returns `None` if no needed piece is available
/// from any peer.
///
/// ## Complexity
///
/// O(peers × piece_count + needed × log(needed)). For typical sizes
/// (≤200 peers, ≤10k pieces), this completes in microseconds.
pub fn select_next_piece(
    needed: &[u32],
    bitfields: &[&PeerBitfield],
    priority_map: &PiecePriorityMap,
    piece_count: u32,
) -> Option<PieceSelection> {
    if needed.is_empty() {
        return None;
    }

    // Step 1: Compute per-piece rarity.
    let scores = rarity_scores(bitfields, piece_count);
    let max_rarity = scores.iter().copied().max().unwrap_or(0);

    // Step 2: Score each needed piece.
    let mut candidates: Vec<PieceSelection> = needed
        .iter()
        .filter_map(|&idx| {
            // Only consider pieces that at least one peer has.
            let rarity = scores.get(idx as usize).copied().unwrap_or(0);
            if rarity == 0 {
                return None;
            }
            let score = priority_map.weighted_score(idx, rarity, max_rarity);
            Some(PieceSelection {
                piece_index: idx,
                score,
            })
        })
        .collect();

    // Step 3: Sort by score descending, then by piece index ascending
    // (streaming bias: prefer lower indices when scores are equal).
    candidates.sort_by(|a, b| {
        b.score
            .cmp(&a.score)
            .then(a.piece_index.cmp(&b.piece_index))
    });

    // Step 4: Return the first candidate.
    candidates.first().copied()
}

/// Selects up to `count` pieces in priority order.
///
/// Useful for batch-requesting multiple pieces at once (parallel downloads).
/// Each returned piece is the next-best choice after the previous ones.
pub fn select_multiple_pieces(
    needed: &[u32],
    bitfields: &[&PeerBitfield],
    priority_map: &PiecePriorityMap,
    piece_count: u32,
    count: usize,
) -> Vec<PieceSelection> {
    if needed.is_empty() || count == 0 {
        return Vec::new();
    }

    let scores = rarity_scores(bitfields, piece_count);
    let max_rarity = scores.iter().copied().max().unwrap_or(0);

    let mut candidates: Vec<PieceSelection> = needed
        .iter()
        .filter_map(|&idx| {
            let rarity = scores.get(idx as usize).copied().unwrap_or(0);
            if rarity == 0 {
                return None;
            }
            let score = priority_map.weighted_score(idx, rarity, max_rarity);
            Some(PieceSelection {
                piece_index: idx,
                score,
            })
        })
        .collect();

    candidates.sort_by(|a, b| {
        b.score
            .cmp(&a.score)
            .then(a.piece_index.cmp(&b.piece_index))
    });

    candidates.truncate(count);
    candidates
}

// ── Tests ───────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;
    use crate::bitfield::PeerBitfield;
    use crate::priority::{PiecePriority, PiecePriorityMap};
    use std::time::Instant;

    // ── Basic rarest-first ──────────────────────────────────────────

    /// Rarest piece is selected when all priorities are equal.
    ///
    /// With uniform Normal priority, the selection should be pure
    /// rarest-first: prefer the piece with fewest replicas.
    #[test]
    fn rarest_piece_selected_at_equal_priority() {
        let now = Instant::now();
        let map = PiecePriorityMap::new(4, now);

        // Peer A has pieces 0,1,2. Peer B has pieces 0,2.
        // Rarity: piece 0=2, piece 1=1, piece 2=2, piece 3=0.
        let mut a = PeerBitfield::new_empty(4);
        a.set_piece(0);
        a.set_piece(1);
        a.set_piece(2);
        let mut b = PeerBitfield::new_empty(4);
        b.set_piece(0);
        b.set_piece(2);

        let needed = vec![0, 1, 2];
        let result = select_next_piece(&needed, &[&a, &b], &map, 4);
        let sel = result.expect("should select a piece");
        // Piece 1 is rarest (only 1 peer has it).
        assert_eq!(sel.piece_index, 1);
    }

    /// Unavailable pieces (rarity 0) are skipped.
    #[test]
    fn unavailable_pieces_skipped() {
        let now = Instant::now();
        let map = PiecePriorityMap::new(4, now);

        let mut a = PeerBitfield::new_empty(4);
        a.set_piece(0);
        // Pieces 1,2,3 have rarity 0 — no peer has them.
        let needed = vec![0, 1, 2, 3];
        let result = select_next_piece(&needed, &[&a], &map, 4);
        let sel = result.expect("should select piece 0");
        assert_eq!(sel.piece_index, 0);
    }

    /// Returns None when no needed piece is available from any peer.
    #[test]
    fn returns_none_when_no_pieces_available() {
        let now = Instant::now();
        let map = PiecePriorityMap::new(4, now);
        let a = PeerBitfield::new_empty(4); // Peer has nothing.
        let needed = vec![0, 1, 2, 3];
        assert!(select_next_piece(&needed, &[&a], &map, 4).is_none());
    }

    /// Empty needed list returns None.
    #[test]
    fn empty_needed_returns_none() {
        let now = Instant::now();
        let map = PiecePriorityMap::new(4, now);
        let a = PeerBitfield::new_full(4);
        assert!(select_next_piece(&[], &[&a], &map, 4).is_none());
    }

    // ── Priority overrides rarity ───────────────────────────────────

    /// Critical priority overrides rarity — a common Critical piece beats
    /// a rare Normal piece.
    ///
    /// This is the DASH/HLS lesson: playback stalls are worse than
    /// suboptimal swarm distribution.
    #[test]
    fn critical_priority_overrides_rarity() {
        let now = Instant::now();
        let mut map = PiecePriorityMap::new(4, now);
        // Piece 0 is Critical (playhead). Piece 1 is Normal.
        map.update(&[0], &[], &[], now);

        // Piece 0: rarity=3 (common). Piece 1: rarity=1 (rare).
        let a = PeerBitfield::new_full(4);
        let mut b = PeerBitfield::new_empty(4);
        b.set_piece(0);
        b.set_piece(1);
        let mut c = PeerBitfield::new_empty(4);
        c.set_piece(0);

        let needed = vec![0, 1];
        let result = select_next_piece(&needed, &[&a, &b, &c], &map, 4);
        let sel = result.expect("should select");
        // Piece 0 wins despite being more common, because it's Critical.
        assert_eq!(sel.piece_index, 0);
    }

    // ── Tie-breaking ────────────────────────────────────────────────

    /// Equal scores tie-break by piece index ascending (streaming bias).
    ///
    /// When two pieces have the same priority and rarity, prefer the
    /// lower-indexed piece. This naturally biases toward sequential
    /// download order, which helps streaming readers.
    #[test]
    fn tie_breaks_by_piece_index_ascending() {
        let now = Instant::now();
        let map = PiecePriorityMap::new(4, now);
        let a = PeerBitfield::new_full(4);
        // All pieces: same rarity (1), same priority (Normal).
        let needed = vec![3, 1, 2, 0];
        let result = select_next_piece(&needed, &[&a], &map, 4);
        let sel = result.expect("should select");
        assert_eq!(sel.piece_index, 0); // Lowest index wins.
    }

    // ── Batch selection ─────────────────────────────────────────────

    /// select_multiple_pieces returns pieces in priority order.
    #[test]
    fn multiple_pieces_in_priority_order() {
        let now = Instant::now();
        let mut map = PiecePriorityMap::new(4, now);
        map.update(&[0], &[1], &[], now);
        let a = PeerBitfield::new_full(4);
        let pieces = select_multiple_pieces(&[0, 1, 2, 3], &[&a], &map, 4, 3);
        assert_eq!(pieces.len(), 3);
        // Piece 0 (Critical) first, piece 1 (High) second.
        assert_eq!(pieces[0].piece_index, 0);
        assert_eq!(pieces[1].piece_index, 1);
    }

    /// select_multiple_pieces respects count limit.
    #[test]
    fn multiple_pieces_respects_count() {
        let now = Instant::now();
        let map = PiecePriorityMap::new(10, now);
        let a = PeerBitfield::new_full(10);
        let needed: Vec<u32> = (0..10).collect();
        let pieces = select_multiple_pieces(&needed, &[&a], &map, 10, 3);
        assert_eq!(pieces.len(), 3);
    }

    /// select_multiple_pieces returns empty for zero count.
    #[test]
    fn multiple_pieces_zero_count() {
        let now = Instant::now();
        let map = PiecePriorityMap::new(4, now);
        let a = PeerBitfield::new_full(4);
        let pieces = select_multiple_pieces(&[0, 1], &[&a], &map, 4, 0);
        assert!(pieces.is_empty());
    }

    // ── Integration: full pipeline ──────────────────────────────────

    /// End-to-end: streaming scenario with mixed priorities and partial peers.
    ///
    /// Simulates a video download where:
    /// - Pieces 0-1 are the playhead (Critical).
    /// - Pieces 2-3 are prebuffer (High).
    /// - Piece 9 is the container index at file end (Critical metadata).
    /// - Peers have different subsets.
    #[test]
    fn streaming_scenario_full_pipeline() {
        let now = Instant::now();
        let mut map = PiecePriorityMap::new(10, now);
        map.update(&[0, 1], &[2, 3], &[9], now);

        // Peer A: has pieces 0-5.
        let mut a = PeerBitfield::new_empty(10);
        for i in 0..6 {
            a.set_piece(i);
        }
        // Peer B: has pieces 3-9.
        let mut b = PeerBitfield::new_empty(10);
        for i in 3..10 {
            b.set_piece(i);
        }

        let needed: Vec<u32> = (0..10).collect();
        let pieces = select_multiple_pieces(&needed, &[&a, &b], &map, 10, 5);

        // First picks should be Critical pieces: 0, 1, 9
        // Then High pieces: 2, 3
        assert!(!pieces.is_empty());
        let first_indices: Vec<u32> = pieces.iter().map(|p| p.piece_index).collect();
        // All Critical pieces should appear before Normal pieces.
        let critical_positions: Vec<usize> = first_indices
            .iter()
            .enumerate()
            .filter(|(_, &idx)| map.get(idx) == PiecePriority::Critical)
            .map(|(pos, _)| pos)
            .collect();
        let non_critical_positions: Vec<usize> = first_indices
            .iter()
            .enumerate()
            .filter(|(_, &idx)| map.get(idx) != PiecePriority::Critical)
            .map(|(pos, _)| pos)
            .collect();
        if let (Some(&last_crit), Some(&first_non)) =
            (critical_positions.last(), non_critical_positions.first())
        {
            assert!(
                last_crit < first_non,
                "Critical pieces should come before non-critical"
            );
        }
    }
}
