// SPDX-License-Identifier: MIT OR Apache-2.0

//! Per-peer piece availability bitfield (BT BEP-3).
//!
//! ## What
//!
//! A compact bitfield that tracks which pieces a specific peer advertises as
//! available. In BitTorrent, each peer sends a `bitfield` message immediately
//! after handshake, then sends `have` messages for subsequently completed
//! pieces. This module provides the data structure for that tracking.
//!
//! ## Why
//!
//! Without per-peer bitfields, the coordinator must assume every peer has every
//! piece. This works for HTTP web seeds (which do have every piece) but breaks
//! in real BT swarms where peers are partial. Worse, it prevents rarest-first
//! piece selection — the single most important algorithm for swarm health.
//!
//! ## How
//!
//! `PeerBitfield` stores one bit per piece in a compact `Vec<u8>`. The BEP-3
//! wire format is big-endian bit ordering: bit 7 of byte 0 is piece 0, bit 6
//! of byte 0 is piece 1, etc. This module uses the same encoding so bitfields
//! can be constructed directly from wire data without conversion.
//!
//! ## Integration
//!
//! - Coordinator stores one `PeerBitfield` per connected peer.
//! - `PeerBitfield::rarity_scores()` produces per-piece availability counts
//!   for rarest-first selection (see [`crate::selection`]).
//! - Web seed peers use `PeerBitfield::new_full()` — they always have
//!   everything.

/// A per-peer piece availability bitfield.
///
/// Stores one bit per piece using BEP-3 big-endian bit ordering:
/// bit 7 of byte 0 = piece 0, bit 6 of byte 0 = piece 1, etc.
///
/// ```
/// use p2p_distribute::PeerBitfield;
///
/// let mut bf = PeerBitfield::new_empty(10);
/// assert!(!bf.has_piece(3));
///
/// bf.set_piece(3);
/// assert!(bf.has_piece(3));
/// assert_eq!(bf.count_have(), 1);
/// ```
#[derive(Debug, Clone)]
pub struct PeerBitfield {
    /// Packed bits, big-endian bit order within each byte (BEP-3 wire format).
    bytes: Vec<u8>,
    /// Total number of pieces this bitfield represents.
    piece_count: u32,
}

impl PeerBitfield {
    /// Creates a bitfield where the peer has no pieces.
    pub fn new_empty(piece_count: u32) -> Self {
        let byte_count = bytes_needed(piece_count);
        Self {
            bytes: vec![0u8; byte_count],
            piece_count,
        }
    }

    /// Creates a bitfield where the peer has all pieces.
    ///
    /// Use this for HTTP web seeds, which always serve every piece.
    /// Spare bits in the last byte are left as zero per BEP-3.
    pub fn new_full(piece_count: u32) -> Self {
        let byte_count = bytes_needed(piece_count);
        let mut bytes = vec![0xFF; byte_count];
        // Clear spare bits in the last byte.
        let spare = byte_count
            .saturating_mul(8)
            .saturating_sub(piece_count as usize);
        if spare > 0 {
            if let Some(last) = bytes.last_mut() {
                // Spare bits are the lowest `spare` bits of the last byte.
                *last &= 0xFF << spare;
            }
        }
        Self { bytes, piece_count }
    }

    /// Creates a bitfield from raw BEP-3 wire bytes.
    ///
    /// Returns `None` if the byte slice length does not match the expected
    /// length for `piece_count`. Spare bits in the last byte are accepted
    /// as-is (BEP-3 says they SHOULD be zero, but implementations vary).
    pub fn from_wire(piece_count: u32, wire_bytes: &[u8]) -> Option<Self> {
        let expected = bytes_needed(piece_count);
        if wire_bytes.len() != expected {
            return None;
        }
        Some(Self {
            bytes: wire_bytes.to_vec(),
            piece_count,
        })
    }

    /// Total number of pieces this bitfield tracks.
    pub fn piece_count(&self) -> u32 {
        self.piece_count
    }

    /// Whether the peer has the given piece.
    ///
    /// Returns `false` for out-of-range indices.
    pub fn has_piece(&self, index: u32) -> bool {
        if index >= self.piece_count {
            return false;
        }
        let byte_idx = (index / 8) as usize;
        let bit_idx = 7 - (index % 8);
        self.bytes
            .get(byte_idx)
            .is_some_and(|b| (b >> bit_idx) & 1 == 1)
    }

    /// Marks a piece as available (BEP-3 `have` message).
    ///
    /// No-op for out-of-range indices.
    pub fn set_piece(&mut self, index: u32) {
        if index >= self.piece_count {
            return;
        }
        let byte_idx = (index / 8) as usize;
        let bit_idx = 7 - (index % 8);
        if let Some(b) = self.bytes.get_mut(byte_idx) {
            *b |= 1 << bit_idx;
        }
    }

    /// Marks a piece as unavailable.
    ///
    /// Not part of standard BT, but useful for testing and for peers that
    /// lose pieces (e.g. storage failure).
    pub fn clear_piece(&mut self, index: u32) {
        if index >= self.piece_count {
            return;
        }
        let byte_idx = (index / 8) as usize;
        let bit_idx = 7 - (index % 8);
        if let Some(b) = self.bytes.get_mut(byte_idx) {
            *b &= !(1 << bit_idx);
        }
    }

    /// Number of pieces the peer has.
    pub fn count_have(&self) -> u32 {
        // count_ones on each byte; subtract spare bits if set.
        let total_ones: u32 = self.bytes.iter().map(|b| b.count_ones()).sum();
        // Spare bits should be zero per BEP-3, but clamp to piece_count
        // defensively in case a misbehaving peer sets them.
        total_ones.min(self.piece_count)
    }

    /// Whether the peer has all pieces.
    pub fn is_complete(&self) -> bool {
        self.count_have() == self.piece_count
    }

    /// Whether the peer has no pieces.
    pub fn is_empty(&self) -> bool {
        self.bytes.iter().all(|b| *b == 0)
    }

    /// Raw bytes in BEP-3 wire format (for serialisation).
    pub fn as_wire_bytes(&self) -> &[u8] {
        &self.bytes
    }
}

// ── Rarity computation ──────────────────────────────────────────────

/// Computes per-piece availability counts from multiple peer bitfields.
///
/// Returns a `Vec<u32>` of length `piece_count` where each element is the
/// number of peers that have that piece. Pieces with lower counts are rarer
/// and should be prioritised by rarest-first selection.
///
/// ## BitTorrent rarest-first rationale
///
/// The rarest piece is the one most likely to become unavailable if a peer
/// disconnects. Downloading it first maximises the probability that every
/// piece survives peer churn, which is the single most important property
/// for swarm health.
///
/// ## Performance
///
/// O(peers × piece_count). For typical swarm sizes (≤200 peers, ≤10k pieces)
/// this completes in microseconds. No allocation beyond the output vector.
pub fn rarity_scores(bitfields: &[&PeerBitfield], piece_count: u32) -> Vec<u32> {
    let mut scores = vec![0u32; piece_count as usize];
    for bf in bitfields {
        for (i, score) in scores.iter_mut().enumerate() {
            if bf.has_piece(i as u32) {
                *score = score.saturating_add(1);
            }
        }
    }
    scores
}

/// Number of bytes needed to store `piece_count` bits.
fn bytes_needed(piece_count: u32) -> usize {
    piece_count.saturating_add(7).checked_div(8).unwrap_or(0) as usize
}

// ── Tests ───────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;

    // ── Construction and basic operations ────────────────────────────

    /// An empty bitfield reports no pieces available.
    #[test]
    fn empty_bitfield_has_no_pieces() {
        let bf = PeerBitfield::new_empty(16);
        assert_eq!(bf.count_have(), 0);
        assert!(bf.is_empty());
        assert!(!bf.is_complete());
    }

    /// A full bitfield reports all pieces available.
    #[test]
    fn full_bitfield_has_all_pieces() {
        let bf = PeerBitfield::new_full(16);
        assert_eq!(bf.count_have(), 16);
        assert!(bf.is_complete());
        assert!(!bf.is_empty());
    }

    /// Full bitfield with non-byte-aligned count handles spare bits.
    ///
    /// BEP-3 says spare bits in the last byte SHOULD be zero. With 10
    /// pieces, bytes needed = 2, spare bits = 6. All 10 pieces must be
    /// set, none of the 6 spare bits.
    #[test]
    fn full_bitfield_spare_bits_are_zero() {
        let bf = PeerBitfield::new_full(10);
        assert_eq!(bf.count_have(), 10);
        assert!(bf.is_complete());
        // Byte 1 should be 0b11000000 (bits 8,9 set, 10-15 clear).
        assert_eq!(bf.as_wire_bytes().get(1).copied(), Some(0b1100_0000));
    }

    /// set_piece and has_piece round-trip correctly.
    #[test]
    fn set_and_has_piece_round_trip() {
        let mut bf = PeerBitfield::new_empty(32);
        bf.set_piece(0);
        bf.set_piece(7);
        bf.set_piece(8);
        bf.set_piece(31);
        assert!(bf.has_piece(0));
        assert!(bf.has_piece(7));
        assert!(bf.has_piece(8));
        assert!(bf.has_piece(31));
        assert!(!bf.has_piece(1));
        assert!(!bf.has_piece(30));
        assert_eq!(bf.count_have(), 4);
    }

    /// clear_piece removes a previously set piece.
    #[test]
    fn clear_piece_removes_bit() {
        let mut bf = PeerBitfield::new_full(8);
        bf.clear_piece(3);
        assert!(!bf.has_piece(3));
        assert_eq!(bf.count_have(), 7);
    }

    /// Out-of-range indices are handled gracefully.
    #[test]
    fn out_of_range_is_safe() {
        let mut bf = PeerBitfield::new_empty(8);
        assert!(!bf.has_piece(8));
        assert!(!bf.has_piece(100));
        bf.set_piece(100); // No-op, no panic.
        bf.clear_piece(100); // No-op, no panic.
        assert_eq!(bf.count_have(), 0);
    }

    // ── Wire format ─────────────────────────────────────────────────

    /// from_wire accepts correctly sized bytes.
    #[test]
    fn from_wire_correct_length() {
        // 16 pieces = 2 bytes. Piece 0 + piece 15 set.
        let wire = [0b1000_0000, 0b0000_0001];
        let bf = PeerBitfield::from_wire(16, &wire).expect("valid wire data");
        assert!(bf.has_piece(0));
        assert!(bf.has_piece(15));
        assert!(!bf.has_piece(1));
        assert_eq!(bf.count_have(), 2);
    }

    /// from_wire rejects wrong-length bytes.
    #[test]
    fn from_wire_wrong_length_returns_none() {
        assert!(PeerBitfield::from_wire(16, &[0u8; 3]).is_none());
        assert!(PeerBitfield::from_wire(16, &[0u8; 1]).is_none());
    }

    /// as_wire_bytes round-trips through from_wire.
    #[test]
    fn wire_bytes_round_trip() {
        let mut bf = PeerBitfield::new_empty(24);
        bf.set_piece(0);
        bf.set_piece(12);
        bf.set_piece(23);
        let wire = bf.as_wire_bytes().to_vec();
        let bf2 = PeerBitfield::from_wire(24, &wire).expect("round trip");
        assert!(bf2.has_piece(0));
        assert!(bf2.has_piece(12));
        assert!(bf2.has_piece(23));
        assert_eq!(bf2.count_have(), 3);
    }

    // ── BEP-3 bit ordering verification ─────────────────────────────

    /// Verifies BEP-3 big-endian bit ordering: piece 0 is bit 7 of byte 0.
    ///
    /// BEP-3 states: "The high bit in the first byte corresponds to piece
    /// index 0." This test validates that invariant directly.
    #[test]
    fn bep3_bit_ordering() {
        let mut bf = PeerBitfield::new_empty(8);
        bf.set_piece(0);
        // Piece 0 = bit 7 of byte 0 = 0b10000000 = 0x80.
        assert_eq!(bf.as_wire_bytes().first().copied(), Some(0x80));

        let mut bf2 = PeerBitfield::new_empty(8);
        bf2.set_piece(7);
        // Piece 7 = bit 0 of byte 0 = 0b00000001 = 0x01.
        assert_eq!(bf2.as_wire_bytes().first().copied(), Some(0x01));
    }

    // ── Rarity scores ───────────────────────────────────────────────

    /// Rarity scores reflect per-piece availability across peers.
    ///
    /// Pieces held by fewer peers get lower scores, making them rarer
    /// and higher priority for rarest-first selection.
    #[test]
    fn rarity_scores_basic() {
        let mut a = PeerBitfield::new_empty(4);
        a.set_piece(0);
        a.set_piece(1);
        a.set_piece(2);

        let mut b = PeerBitfield::new_empty(4);
        b.set_piece(0);
        b.set_piece(2);

        let mut c = PeerBitfield::new_empty(4);
        c.set_piece(0);

        let scores = rarity_scores(&[&a, &b, &c], 4);
        // Piece 0: all 3 peers have it.
        assert_eq!(scores.first().copied(), Some(3));
        // Piece 1: only peer A.
        assert_eq!(scores.get(1).copied(), Some(1));
        // Piece 2: peers A + B.
        assert_eq!(scores.get(2).copied(), Some(2));
        // Piece 3: nobody.
        assert_eq!(scores.get(3).copied(), Some(0));
    }

    /// Rarity scores for empty peer list returns all zeros.
    #[test]
    fn rarity_scores_no_peers() {
        let scores = rarity_scores(&[], 8);
        assert!(scores.iter().all(|s| *s == 0));
        assert_eq!(scores.len(), 8);
    }

    /// Rarity scores for full peers returns uniform counts.
    #[test]
    fn rarity_scores_all_full() {
        let a = PeerBitfield::new_full(4);
        let b = PeerBitfield::new_full(4);
        let scores = rarity_scores(&[&a, &b], 4);
        assert!(scores.iter().all(|s| *s == 2));
    }

    // ── Edge cases ──────────────────────────────────────────────────

    /// Zero-piece bitfield is valid and empty.
    #[test]
    fn zero_pieces_is_valid() {
        let bf = PeerBitfield::new_empty(0);
        assert_eq!(bf.count_have(), 0);
        assert!(bf.is_empty());
        // 0 pieces → is_complete because 0 == 0.
        assert!(bf.is_complete());
    }

    /// Single-piece bitfield works correctly.
    #[test]
    fn single_piece() {
        let mut bf = PeerBitfield::new_empty(1);
        assert!(!bf.has_piece(0));
        bf.set_piece(0);
        assert!(bf.has_piece(0));
        assert_eq!(bf.as_wire_bytes().first().copied(), Some(0x80));
        assert!(bf.is_complete());
    }

    // ── Security: adversarial wire data ─────────────────────────────

    /// Malformed wire bytes with spare bits set are accepted but clamped.
    ///
    /// BEP-3 says spare bits SHOULD be zero, but misbehaving peers may set
    /// them. `from_wire` must accept the bytes (correct length), and
    /// `count_have` must clamp to `piece_count` so spare bits don't inflate
    /// the reported count.
    #[test]
    fn adversarial_spare_bits_set_in_wire_data() {
        // 10 pieces = 2 bytes, 6 spare bits. Set ALL bits including spare.
        let wire = [0xFF, 0xFF];
        let bf = PeerBitfield::from_wire(10, &wire).expect("correct length accepted");
        // count_have must report at most 10, not 16.
        assert_eq!(bf.count_have(), 10);
        assert!(bf.is_complete());
    }

    /// Empty wire bytes (0 length) for non-zero piece count are rejected.
    ///
    /// A peer claiming to have pieces but sending zero bytes is malformed.
    #[test]
    fn adversarial_empty_wire_for_nonzero_pieces() {
        assert!(PeerBitfield::from_wire(8, &[]).is_none());
        assert!(PeerBitfield::from_wire(1, &[]).is_none());
    }

    /// Oversized wire bytes are rejected.
    ///
    /// A peer sending more bytes than expected could be attempting buffer
    /// overflow. `from_wire` rejects any length mismatch.
    #[test]
    fn adversarial_oversized_wire_rejected() {
        // 8 pieces needs 1 byte, not 2.
        assert!(PeerBitfield::from_wire(8, &[0xFF, 0xFF]).is_none());
        // 16 pieces needs 2 bytes, not 100.
        assert!(PeerBitfield::from_wire(16, &[0u8; 100]).is_none());
    }

    /// Wire bytes with all zeros are valid (peer has nothing).
    ///
    /// A newly-connected leecher sends an all-zero bitfield.
    #[test]
    fn adversarial_all_zero_wire_is_valid_empty() {
        let bf = PeerBitfield::from_wire(16, &[0, 0]).expect("valid");
        assert!(bf.is_empty());
        assert_eq!(bf.count_have(), 0);
    }

    /// `from_wire` with `piece_count = 0` and empty bytes succeeds.
    ///
    /// Edge case: a torrent with zero pieces. The bitfield is zero bytes.
    #[test]
    fn adversarial_zero_pieces_zero_bytes() {
        let bf = PeerBitfield::from_wire(0, &[]).expect("valid");
        assert!(bf.is_complete());
        assert!(bf.is_empty());
    }

    /// Large piece count does not cause allocation panic.
    ///
    /// Untrusted peer announces a very large piece count. `bytes_needed`
    /// must not overflow. We test with a sane upper bound — actual
    /// BitTorrent torrents rarely exceed 100k pieces.
    #[test]
    fn adversarial_large_piece_count_no_panic() {
        // 1 million pieces = 125,000 bytes — should not panic.
        let bf = PeerBitfield::new_empty(1_000_000);
        assert_eq!(bf.piece_count(), 1_000_000);
        assert_eq!(bf.count_have(), 0);
    }
}
